//! Configuration and shared data types for the file-routing core.
//!
//! Everything that governs how a `routes/` and `api/` tree is interpreted is
//! expressed as data on [`FileRoutingConfig`], with a [`Default`] that encodes
//! Treaty's conventions and a [`FileRoutingConfig::resolve`] that overlays a
//! caller-supplied [`PartialFileRoutingConfig`] (the shape a NAPI / TS binding
//! will hand in) onto those defaults.
//!
//! The scanner, route builder, and api builder all read this config and emit
//! the serde-serializable output types defined here so a later NAPI layer can
//! surface them verbatim.

use serde::{Deserialize, Serialize};

/// Default name of the directory holding page/layout routes.
pub const DEFAULT_ROUTES_DIR: &str = "routes";
/// Default name of the directory holding server endpoints.
pub const DEFAULT_API_DIR: &str = "api";
/// Default file name (sans extension) treated as a layout for its directory.
pub const DEFAULT_LAYOUT_FILE: &str = "layout";
/// Default file name (sans extension) treated as the 404 / wildcard route.
pub const DEFAULT_NOT_FOUND_FILE: &str = "not-found";

/// File base names (sans extension) that resolve to a directory's index route.
pub fn default_index_file_names() -> Vec<String> {
    vec!["index".to_string(), "page".to_string()]
}

/// Extensions (leading dot included) recognised as routable component files.
pub fn default_route_extensions() -> Vec<String> {
    vec![
        ".treaty".to_string(),
        ".tjsx".to_string(),
        ".tsx".to_string(),
        ".ts".to_string(),
    ]
}

/// Extensions (leading dot included) recognised as server endpoint files.
pub fn default_api_extensions() -> Vec<String> {
    vec![".treaty".to_string(), ".ts".to_string()]
}

/// How a dynamic path segment is spelled in a file/directory name.
///
/// Both styles are *always* parsed on input; this only selects the canonical
/// style the resolved config advertises. `Bracket` matches `[param]`,
/// `Colon` matches `:param`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DynamicSegmentStyle {
    /// `[param]` — the default, filesystem-safe form.
    #[default]
    Bracket,
    /// `:param` — the Angular route form.
    Colon,
}

/// Fully-resolved file-routing configuration.
///
/// Construct via [`FileRoutingConfig::default`] for the conventions, or
/// [`FileRoutingConfig::resolve`] to overlay caller overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRoutingConfig {
    /// Master switch; when `false`, [`crate::generate_routing`] yields empty output.
    pub enabled: bool,
    /// Project root the `routes_dir` / `api_dir` are resolved against. Empty = tree root.
    pub root_dir: String,
    /// Directory name holding routable pages/layouts. Default `"routes"`.
    pub routes_dir: String,
    /// Directory name holding server endpoints. Default `"api"`.
    pub api_dir: String,
    /// Canonical spelling for dynamic segments on output. Both styles parse on input.
    pub dynamic_segment_style: DynamicSegmentStyle,
    /// Base names that mean "this directory's index route". Default `["index", "page"]`.
    pub index_file_names: Vec<String>,
    /// Base name treated as a layout for its directory. Default `"layout"`.
    pub layout_file_name: String,
    /// Base name treated as the 404 / wildcard route. Default `"not-found"`.
    pub not_found_file_name: String,
    /// Recognised route file extensions, in precedence order. Leading dots included.
    pub route_extensions: Vec<String>,
    /// Recognised api file extensions, in precedence order. Leading dots included.
    pub api_extensions: Vec<String>,
    /// Emit Module Federation remote descriptors alongside routes. Default `true`.
    pub federation: bool,
}

impl Default for FileRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            root_dir: String::new(),
            routes_dir: DEFAULT_ROUTES_DIR.to_string(),
            api_dir: DEFAULT_API_DIR.to_string(),
            dynamic_segment_style: DynamicSegmentStyle::default(),
            index_file_names: default_index_file_names(),
            layout_file_name: DEFAULT_LAYOUT_FILE.to_string(),
            not_found_file_name: DEFAULT_NOT_FOUND_FILE.to_string(),
            route_extensions: default_route_extensions(),
            api_extensions: default_api_extensions(),
            federation: true,
        }
    }
}

/// Caller-supplied overrides; every field is optional. This is the shape a
/// NAPI / TS binding deserializes from a JS object and hands to
/// [`FileRoutingConfig::resolve`]. Absent fields fall back to the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PartialFileRoutingConfig {
    pub enabled: Option<bool>,
    pub root_dir: Option<String>,
    pub routes_dir: Option<String>,
    pub api_dir: Option<String>,
    pub dynamic_segment_style: Option<DynamicSegmentStyle>,
    pub index_file_names: Option<Vec<String>>,
    pub layout_file_name: Option<String>,
    pub not_found_file_name: Option<String>,
    pub route_extensions: Option<Vec<String>>,
    pub api_extensions: Option<Vec<String>>,
    pub federation: Option<bool>,
}

impl FileRoutingConfig {
    /// Overlay `partial` onto the defaults. Any `Some(_)` field replaces the
    /// default; any `None` keeps it. Deterministic and allocation-cheap.
    pub fn resolve(partial: PartialFileRoutingConfig) -> Self {
        let base = Self::default();
        Self {
            enabled: partial.enabled.unwrap_or(base.enabled),
            root_dir: partial.root_dir.unwrap_or(base.root_dir),
            routes_dir: partial.routes_dir.unwrap_or(base.routes_dir),
            api_dir: partial.api_dir.unwrap_or(base.api_dir),
            dynamic_segment_style: partial
                .dynamic_segment_style
                .unwrap_or(base.dynamic_segment_style),
            index_file_names: partial.index_file_names.unwrap_or(base.index_file_names),
            layout_file_name: partial.layout_file_name.unwrap_or(base.layout_file_name),
            not_found_file_name: partial
                .not_found_file_name
                .unwrap_or(base.not_found_file_name),
            route_extensions: partial.route_extensions.unwrap_or(base.route_extensions),
            api_extensions: partial.api_extensions.unwrap_or(base.api_extensions),
            federation: partial.federation.unwrap_or(base.federation),
        }
    }

    /// If `name` ends with one of the configured `route_extensions`, return the
    /// base name (extension stripped) plus the matched extension; else `None`.
    /// Longest-extension-first so `.tjsx` wins over a hypothetical `.jsx`.
    pub fn match_route_extension<'a>(&self, name: &'a str) -> Option<(&'a str, String)> {
        match_extension(name, &self.route_extensions)
    }

    /// As [`Self::match_route_extension`] but against `api_extensions`.
    pub fn match_api_extension<'a>(&self, name: &'a str) -> Option<(&'a str, String)> {
        match_extension(name, &self.api_extensions)
    }
}

/// Strip the longest matching extension from `name`. Returns `(stem, ext)`.
fn match_extension<'a>(name: &'a str, exts: &[String]) -> Option<(&'a str, String)> {
    exts.iter()
        .filter(|ext| name.len() > ext.len() && name.ends_with(ext.as_str()))
        .max_by_key(|ext| ext.len())
        .map(|ext| (&name[..name.len() - ext.len()], ext.clone()))
}

// ---------------------------------------------------------------------------
// Directory-tree abstraction (injectable; in-memory in tests, real-FS later).
// ---------------------------------------------------------------------------

/// Kind of a directory entry as reported by a [`DirTree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
}

/// A single child of a directory: its bare name and whether it is a file or dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Bare entry name (no path separators), e.g. `"index.treaty"` or `"users"`.
    pub name: String,
    /// Whether this entry is a file or a directory.
    pub kind: EntryKind,
}

impl Entry {
    pub fn file(name: impl Into<String>) -> Self {
        Self { name: name.into(), kind: EntryKind::File }
    }
    pub fn dir(name: impl Into<String>) -> Self {
        Self { name: name.into(), kind: EntryKind::Dir }
    }
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }
}

/// Read-only directory abstraction injected into the scanner so the core logic
/// is exercised with in-memory fixtures and never touches a real filesystem.
///
/// `path` is always tree-relative and `/`-separated (the empty string is the
/// tree root). Implementations return the direct children of `path`; the order
/// is implementation-defined — the scanner sorts for determinism.
pub trait DirTree {
    /// Direct children of `path`. A missing or non-directory `path` yields `[]`.
    fn entries(&self, path: &str) -> Vec<Entry>;
}

// ---------------------------------------------------------------------------
// Scanner output types.
// ---------------------------------------------------------------------------

/// A node in the scanned `routes/` tree, before Angular semantics are applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteNode {
    /// Tree-relative `/`-separated path of the directory this node represents.
    pub dir_path: String,
    /// URL path segment contributed by this directory (`""` for the routes root).
    /// Static dirs contribute their name; `[id]` / `:id` contribute a `:id` param.
    pub segment: String,
    /// `true` when `segment` is a dynamic parameter (e.g. from `[id]`).
    pub is_dynamic: bool,
    /// Name of the dynamic parameter when `is_dynamic`, else `None`.
    pub param_name: Option<String>,
    /// Index/page file for this directory, if present (tree-relative path).
    pub index_file: Option<String>,
    /// Layout file wrapping this directory's children, if present.
    pub layout_file: Option<String>,
    /// Not-found / wildcard file for this directory, if present.
    pub not_found_file: Option<String>,
    /// Non-index routable leaf files in this directory (tree-relative paths).
    pub page_files: Vec<RoutePage>,
    /// Child directory nodes, sorted for determinism.
    pub children: Vec<RouteNode>,
}

/// A non-index routable leaf file within a [`RouteNode`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePage {
    /// Tree-relative path to the file.
    pub file_path: String,
    /// URL segment derived from the file's base name (dynamic-aware).
    pub segment: String,
    /// `true` when `segment` is a dynamic parameter.
    pub is_dynamic: bool,
    /// Parameter name when dynamic, else `None`.
    pub param_name: Option<String>,
}

/// A node in the scanned `api/` tree, before endpoint semantics are applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiNode {
    /// Tree-relative `/`-separated path of the directory this node represents.
    pub dir_path: String,
    /// URL path segment contributed by this directory (`""` for the api root).
    pub segment: String,
    /// `true` when `segment` is a dynamic parameter.
    pub is_dynamic: bool,
    /// Parameter name when dynamic, else `None`.
    pub param_name: Option<String>,
    /// Endpoint files directly in this directory (tree-relative paths).
    pub handler_files: Vec<ApiHandlerFile>,
    /// Child directory nodes, sorted for determinism.
    pub children: Vec<ApiNode>,
}

/// An endpoint handler file within an [`ApiNode`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiHandlerFile {
    /// Tree-relative path to the file.
    pub file_path: String,
    /// URL segment derived from the file's base name (dynamic-aware). An index
    /// handler contributes `""`.
    pub segment: String,
    /// `true` when this handler is a directory index (no extra segment).
    pub is_index: bool,
    /// `true` when `segment` is a dynamic parameter.
    pub is_dynamic: bool,
    /// Parameter name when dynamic, else `None`.
    pub param_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Routes output types.
// ---------------------------------------------------------------------------

/// A lazily-loaded Angular route, mirroring the `Route` config object shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AngularRoute {
    /// URL path for this route relative to its parent (`""` for an index route,
    /// `"**"` for a not-found route, `":id"` for a dynamic segment).
    pub path: String,
    /// Tree-relative path of the component file backing this route, if any.
    /// `None` for a purely structural (grouping) route.
    pub component_file: Option<String>,
    /// Tree-relative path of the layout component wrapping `children`, if any.
    pub layout_file: Option<String>,
    /// Whether this route is a wildcard (`path == "**"`).
    pub is_wildcard: bool,
    /// Federation remote name this route is exposed as, when federation is on.
    pub remote_name: Option<String>,
    /// Child routes nested under this route.
    pub children: Vec<AngularRoute>,
}

/// A Module Federation remote descriptor derived from a route subtree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FederationRemote {
    /// Stable remote name (slugified route path).
    pub name: String,
    /// Exposed module key (e.g. `"./Route"`).
    pub exposed_module: String,
    /// Tree-relative path of the entry component file for this remote.
    pub entry_file: String,
    /// URL path the remote is mounted at.
    pub route_path: String,
}

// ---------------------------------------------------------------------------
// Api output types.
// ---------------------------------------------------------------------------

/// A resolved server endpoint mapped from an `api/` handler file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    /// URL path for the endpoint, leading slash, dynamic segments as `:param`.
    pub path: String,
    /// Tree-relative path of the handler file.
    pub handler_file: String,
    /// Ordered names of dynamic parameters appearing in `path`.
    pub param_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// Top-level generated output.
// ---------------------------------------------------------------------------

/// The complete, serde-serializable result of [`crate::generate_routing`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedRouting {
    /// Top-level Angular routes (lazy, file-derived).
    pub routes: Vec<AngularRoute>,
    /// Module Federation remotes (empty when `federation` is disabled).
    pub remotes: Vec<FederationRemote>,
    /// Server endpoint manifest from the `api/` tree.
    pub endpoints: Vec<ApiEndpoint>,
}
