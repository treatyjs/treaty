//! Stage 1: scan a `routes/` or `api/` directory tree into raw nodes.
//!
//! The scanner walks the injected [`DirTree`] starting at the configured
//! directory, classifies each entry (index / layout / not-found / page /
//! handler / dynamic vs static) per the [`FileRoutingConfig`] conventions, and
//! emits a [`RouteNode`] / [`ApiNode`] tree. It applies no Angular or endpoint
//! semantics — that is the job of [`crate::routes`] and [`crate::api`].
//!
//! Children are sorted so output is deterministic regardless of `DirTree` order.

use crate::config::{
    ApiHandlerFile, ApiNode, DirTree, EntryKind, FileRoutingConfig, RouteNode, RoutePage,
};

/// Scan the `routes/` subtree. Returns the root [`RouteNode`] when the routes
/// directory exists under the configured `root_dir`, else `None`.
///
/// The root node carries an empty `segment` (it contributes nothing to the
/// URL); recursion descends into every child directory, skipping the sibling
/// `api_dir` only at the top level (where routes and api share a parent).
pub fn scan_routes(config: &FileRoutingConfig, tree: &dyn DirTree) -> Option<RouteNode> {
    let routes_root = join_path(&config.root_dir, &config.routes_dir);
    if !dir_exists(tree, &config.root_dir, &config.routes_dir) {
        return None;
    }
    Some(scan_route_dir(config, tree, &routes_root, ""))
}

/// Scan the `api/` subtree. Returns the root [`ApiNode`] when the api directory
/// exists under the configured `root_dir`, else `None`.
pub fn scan_api(config: &FileRoutingConfig, tree: &dyn DirTree) -> Option<ApiNode> {
    let api_root = join_path(&config.root_dir, &config.api_dir);
    if !dir_exists(tree, &config.root_dir, &config.api_dir) {
        return None;
    }
    Some(scan_api_dir(config, tree, &api_root, ""))
}

/// Classify a directory/file base name into a URL segment, recognising both
/// `[param]` and `:param` dynamic forms, including catch-all (`[...slug]` /
/// `:...slug`). The emitted `segment` is normalised to the config's
/// [`DynamicSegmentStyle`]; for catch-all params a leading `...` is preserved
/// inside the segment so downstream lowering can map it to an Angular `**`
/// wildcard while still recovering the parameter name.
///
/// Returns `(segment, is_dynamic, param_name)` wrapped in [`SegmentClass`].
/// Route groups — names wrapped in `(parens)` — are *not* a dynamic form; they
/// are reported via [`is_route_group`] separately and classified here as a
/// plain static segment equal to the group name.
pub fn classify_segment(config: &FileRoutingConfig, base_name: &str) -> SegmentClass {
    if let Some(inner) = parse_dynamic(base_name) {
        let (param_name, is_catch_all) = match inner.strip_prefix("...") {
            Some(rest) => (rest.to_string(), true),
            None => (inner.to_string(), false),
        };
        let segment = render_dynamic(config, &param_name, is_catch_all);
        return SegmentClass {
            segment,
            is_dynamic: true,
            is_catch_all,
            param_name: Some(param_name),
        };
    }
    SegmentClass {
        segment: base_name.to_string(),
        is_dynamic: false,
        is_catch_all: false,
        param_name: None,
    }
}

/// Result of classifying a directory or file base name as a URL segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentClass {
    /// URL segment contributed (already normalised to the output param style).
    pub segment: String,
    /// Whether the segment is a dynamic parameter.
    pub is_dynamic: bool,
    /// Whether the segment is a catch-all (`[...slug]` / `:...slug`).
    pub is_catch_all: bool,
    /// Parameter name when dynamic, else `None`.
    pub param_name: Option<String>,
}

/// Whether `base_name` is a route group — a directory wrapped in parentheses,
/// e.g. `(marketing)`. Route groups organise files without contributing a URL
/// segment, so the scanner records them but downstream lowering flattens them.
pub fn is_route_group(base_name: &str) -> bool {
    base_name.len() >= 2 && base_name.starts_with('(') && base_name.ends_with(')')
}

// ---------------------------------------------------------------------------
// Routes walk.
// ---------------------------------------------------------------------------

/// Recursively scan a single routes directory at tree-relative `dir_path`,
/// whose own contributed URL segment derives from `dir_base` (`""` at the root).
fn scan_route_dir(
    config: &FileRoutingConfig,
    tree: &dyn DirTree,
    dir_path: &str,
    dir_base: &str,
) -> RouteNode {
    let class = if dir_base.is_empty() {
        // The routes root contributes no segment.
        SegmentClass {
            segment: String::new(),
            is_dynamic: false,
            is_catch_all: false,
            param_name: None,
        }
    } else if is_route_group(dir_base) {
        // Route groups contribute no URL segment but keep their name visible
        // (parenthesised) so callers can detect and flatten them.
        SegmentClass {
            segment: dir_base.to_string(),
            is_dynamic: false,
            is_catch_all: false,
            param_name: None,
        }
    } else {
        classify_segment(config, dir_base)
    };

    let mut entries = tree.entries(dir_path);
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let mut index_file: Option<String> = None;
    let mut layout_file: Option<String> = None;
    let mut not_found_file: Option<String> = None;
    let mut page_files: Vec<RoutePage> = Vec::new();
    let mut children: Vec<RouteNode> = Vec::new();

    for entry in &entries {
        let child_path = join_path(dir_path, &entry.name);
        match entry.kind {
            EntryKind::Dir => {
                children.push(scan_route_dir(config, tree, &child_path, &entry.name));
            }
            EntryKind::File => {
                let Some((stem, _ext)) = config.match_route_extension(&entry.name) else {
                    continue; // Not a routable extension; ignore.
                };
                if config.index_file_names.iter().any(|n| n == stem) {
                    // First index/page wins (extensions already precedence-sorted
                    // by the deterministic name ordering above is incidental — we
                    // keep the first match for stability).
                    if index_file.is_none() {
                        index_file = Some(child_path);
                    }
                } else if stem == config.layout_file_name {
                    if layout_file.is_none() {
                        layout_file = Some(child_path);
                    }
                } else if stem == config.not_found_file_name {
                    if not_found_file.is_none() {
                        not_found_file = Some(child_path);
                    }
                } else {
                    let page_class = classify_segment(config, stem);
                    page_files.push(RoutePage {
                        file_path: child_path,
                        segment: page_class.segment,
                        is_dynamic: page_class.is_dynamic,
                        param_name: page_class.param_name,
                    });
                }
            }
        }
    }

    // Already iterated in sorted order, so page_files and children are sorted.
    RouteNode {
        dir_path: dir_path.to_string(),
        segment: class.segment,
        is_dynamic: class.is_dynamic,
        param_name: class.param_name,
        index_file,
        layout_file,
        not_found_file,
        page_files,
        children,
    }
}

// ---------------------------------------------------------------------------
// Api walk.
// ---------------------------------------------------------------------------

/// Recursively scan a single api directory at tree-relative `dir_path`, whose
/// own contributed URL segment derives from `dir_base` (`""` at the root).
fn scan_api_dir(
    config: &FileRoutingConfig,
    tree: &dyn DirTree,
    dir_path: &str,
    dir_base: &str,
) -> ApiNode {
    let class = if dir_base.is_empty() {
        SegmentClass {
            segment: String::new(),
            is_dynamic: false,
            is_catch_all: false,
            param_name: None,
        }
    } else if is_route_group(dir_base) {
        SegmentClass {
            segment: dir_base.to_string(),
            is_dynamic: false,
            is_catch_all: false,
            param_name: None,
        }
    } else {
        classify_segment(config, dir_base)
    };

    let mut entries = tree.entries(dir_path);
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let mut handler_files: Vec<ApiHandlerFile> = Vec::new();
    let mut children: Vec<ApiNode> = Vec::new();

    for entry in &entries {
        let child_path = join_path(dir_path, &entry.name);
        match entry.kind {
            EntryKind::Dir => {
                children.push(scan_api_dir(config, tree, &child_path, &entry.name));
            }
            EntryKind::File => {
                let Some((stem, _ext)) = config.match_api_extension(&entry.name) else {
                    continue;
                };
                let is_index = config.index_file_names.iter().any(|n| n == stem);
                if is_index {
                    handler_files.push(ApiHandlerFile {
                        file_path: child_path,
                        segment: String::new(),
                        is_index: true,
                        is_dynamic: false,
                        param_name: None,
                    });
                } else {
                    let seg = classify_segment(config, stem);
                    handler_files.push(ApiHandlerFile {
                        file_path: child_path,
                        segment: seg.segment,
                        is_index: false,
                        is_dynamic: seg.is_dynamic,
                        param_name: seg.param_name,
                    });
                }
            }
        }
    }

    ApiNode {
        dir_path: dir_path.to_string(),
        segment: class.segment,
        is_dynamic: class.is_dynamic,
        param_name: class.param_name,
        handler_files,
        children,
    }
}

// ---------------------------------------------------------------------------
// Segment parsing helpers.
// ---------------------------------------------------------------------------

/// Extract the inner parameter text of a dynamic name in either supported form:
/// `[param]` / `[...slug]` (bracket) or `:param` / `:...slug` (colon). Returns
/// `None` for static names. The inner text retains a leading `...` for
/// catch-all so the caller can split it off.
fn parse_dynamic(base_name: &str) -> Option<&str> {
    if let Some(rest) = base_name.strip_prefix('[') {
        if let Some(inner) = rest.strip_suffix(']') {
            if !inner.is_empty() && !inner.contains('[') && !inner.contains(']') {
                return Some(inner);
            }
        }
        return None;
    }
    if let Some(inner) = base_name.strip_prefix(':') {
        if !inner.is_empty() {
            return Some(inner);
        }
    }
    None
}

/// Render a dynamic parameter back to a URL segment in the config's canonical
/// [`DynamicSegmentStyle`]. Catch-all params keep a `...` prefix on the name so
/// downstream lowering can recognise them.
fn render_dynamic(config: &FileRoutingConfig, param_name: &str, is_catch_all: bool) -> String {
    use crate::config::DynamicSegmentStyle::{Bracket, Colon};
    let body = if is_catch_all {
        format!("...{param_name}")
    } else {
        param_name.to_string()
    };
    match config.dynamic_segment_style {
        Bracket => format!("[{body}]"),
        Colon => format!(":{body}"),
    }
}

// ---------------------------------------------------------------------------
// Path helpers.
// ---------------------------------------------------------------------------

/// Join two tree-relative path components with `/`, treating an empty `base` as
/// the tree root (so the result is just `segment`).
fn join_path(base: &str, segment: &str) -> String {
    match (base.is_empty(), segment.is_empty()) {
        (true, _) => segment.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}/{segment}"),
    }
}

/// Whether `dir_name` exists as a directory entry directly under `parent`.
fn dir_exists(tree: &dyn DirTree, parent: &str, dir_name: &str) -> bool {
    tree.entries(parent)
        .iter()
        .any(|e| e.kind == EntryKind::Dir && e.name == dir_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DynamicSegmentStyle, Entry, PartialFileRoutingConfig};
    use std::collections::HashMap;

    /// Minimal in-memory [`DirTree`] for fixtures: path -> direct children.
    #[derive(Default)]
    struct MemTree {
        dirs: HashMap<String, Vec<Entry>>,
    }

    impl MemTree {
        fn with(mut self, path: &str, entries: Vec<Entry>) -> Self {
            self.dirs.insert(path.to_string(), entries);
            self
        }
    }

    impl DirTree for MemTree {
        fn entries(&self, path: &str) -> Vec<Entry> {
            self.dirs.get(path).cloned().unwrap_or_default()
        }
    }

    fn cfg() -> FileRoutingConfig {
        FileRoutingConfig::default()
    }

    // --- classify_segment ---------------------------------------------------

    #[test]
    fn classify_static_segment() {
        let c = cfg();
        let s = classify_segment(&c, "users");
        assert_eq!(
            s,
            SegmentClass {
                segment: "users".to_string(),
                is_dynamic: false,
                is_catch_all: false,
                param_name: None,
            }
        );
    }

    #[test]
    fn classify_bracket_dynamic() {
        let c = cfg();
        let s = classify_segment(&c, "[id]");
        assert!(s.is_dynamic);
        assert!(!s.is_catch_all);
        assert_eq!(s.param_name.as_deref(), Some("id"));
        // Default canonical style is bracket.
        assert_eq!(s.segment, "[id]");
    }

    #[test]
    fn classify_colon_dynamic_input_normalises_to_bracket() {
        let c = cfg();
        let s = classify_segment(&c, ":slug");
        assert!(s.is_dynamic);
        assert!(!s.is_catch_all);
        assert_eq!(s.param_name.as_deref(), Some("slug"));
        assert_eq!(s.segment, "[slug]");
    }

    #[test]
    fn classify_dynamic_renders_in_colon_style_when_configured() {
        let c = FileRoutingConfig::resolve(PartialFileRoutingConfig {
            dynamic_segment_style: Some(DynamicSegmentStyle::Colon),
            ..Default::default()
        });
        let bracket_in = classify_segment(&c, "[id]");
        assert_eq!(bracket_in.segment, ":id");
        let colon_in = classify_segment(&c, ":id");
        assert_eq!(colon_in.segment, ":id");
    }

    #[test]
    fn classify_catch_all_bracket() {
        let c = cfg();
        let s = classify_segment(&c, "[...slug]");
        assert!(s.is_dynamic);
        assert!(s.is_catch_all);
        assert_eq!(s.param_name.as_deref(), Some("slug"));
        assert_eq!(s.segment, "[...slug]");
    }

    #[test]
    fn classify_catch_all_colon() {
        let c = cfg();
        let s = classify_segment(&c, ":...rest");
        assert!(s.is_dynamic);
        assert!(s.is_catch_all);
        assert_eq!(s.param_name.as_deref(), Some("rest"));
        assert_eq!(s.segment, "[...rest]");
    }

    #[test]
    fn classify_empty_brackets_is_static() {
        let c = cfg();
        let s = classify_segment(&c, "[]");
        assert!(!s.is_dynamic);
        assert_eq!(s.segment, "[]");
    }

    #[test]
    fn route_group_detection() {
        assert!(is_route_group("(marketing)"));
        assert!(is_route_group("()"));
        assert!(!is_route_group("marketing"));
        assert!(!is_route_group("(open"));
        assert!(!is_route_group("close)"));
    }

    // --- scan_routes: existence / separation --------------------------------

    #[test]
    fn missing_routes_dir_yields_none() {
        let c = cfg();
        let tree = MemTree::default().with("", vec![Entry::dir("api")]);
        assert!(scan_routes(&c, &tree).is_none());
    }

    #[test]
    fn missing_api_dir_yields_none() {
        let c = cfg();
        let tree = MemTree::default().with("", vec![Entry::dir("routes")]);
        assert!(scan_api(&c, &tree).is_none());
    }

    #[test]
    fn routes_and_api_are_scanned_independently() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes"), Entry::dir("api")])
            .with("routes", vec![Entry::file("index.treaty")])
            .with("api", vec![Entry::file("users.ts")]);

        let routes = scan_routes(&c, &tree).expect("routes root");
        // The routes scan must not pull in anything from the api dir.
        assert_eq!(routes.dir_path, "routes");
        assert_eq!(routes.index_file.as_deref(), Some("routes/index.treaty"));
        assert!(routes.children.is_empty());
        assert!(routes.page_files.is_empty());

        let api = scan_api(&c, &tree).expect("api root");
        assert_eq!(api.dir_path, "api");
        assert_eq!(api.handler_files.len(), 1);
        assert_eq!(api.handler_files[0].file_path, "api/users.ts");
        // The api scan must not see routes/index.treaty.
        assert!(api.children.is_empty());
    }

    // --- scan_routes: index / layout / not-found ----------------------------

    #[test]
    fn index_layout_not_found_resolution() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes")])
            .with(
                "routes",
                vec![
                    Entry::file("index.treaty"),
                    Entry::file("layout.treaty"),
                    Entry::file("not-found.treaty"),
                    Entry::file("about.treaty"),
                    Entry::file("README.md"), // ignored: wrong extension
                ],
            );
        let root = scan_routes(&c, &tree).unwrap();
        assert_eq!(root.index_file.as_deref(), Some("routes/index.treaty"));
        assert_eq!(root.layout_file.as_deref(), Some("routes/layout.treaty"));
        assert_eq!(
            root.not_found_file.as_deref(),
            Some("routes/not-found.treaty")
        );
        assert_eq!(root.page_files.len(), 1);
        assert_eq!(root.page_files[0].file_path, "routes/about.treaty");
        assert_eq!(root.page_files[0].segment, "about");
        assert!(!root.page_files[0].is_dynamic);
    }

    #[test]
    fn page_alias_resolves_as_index() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes")])
            .with("routes", vec![Entry::file("page.tsx")]);
        let root = scan_routes(&c, &tree).unwrap();
        assert_eq!(root.index_file.as_deref(), Some("routes/page.tsx"));
        assert!(root.page_files.is_empty());
    }

    // --- scan_routes: nesting + dynamic + catch-all -------------------------

    #[test]
    fn nested_dynamic_and_catch_all_directories() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes")])
            .with(
                "routes",
                vec![Entry::dir("users"), Entry::dir("[...slug]")],
            )
            .with(
                "routes/users",
                vec![Entry::file("index.treaty"), Entry::dir("[id]")],
            )
            .with("routes/users/[id]", vec![Entry::file("index.treaty")])
            .with("routes/[...slug]", vec![Entry::file("index.treaty")]);

        let root = scan_routes(&c, &tree).unwrap();
        assert_eq!(root.segment, "");
        // Sorted: "[...slug]" sorts before "users" ('[' < 'u').
        assert_eq!(root.children.len(), 2);

        let slug = &root.children[0];
        assert_eq!(slug.dir_path, "routes/[...slug]");
        assert!(slug.is_dynamic);
        assert_eq!(slug.segment, "[...slug]");
        assert_eq!(slug.param_name.as_deref(), Some("slug"));

        let users = &root.children[1];
        assert_eq!(users.dir_path, "routes/users");
        assert!(!users.is_dynamic);
        assert_eq!(users.segment, "users");
        assert_eq!(users.index_file.as_deref(), Some("routes/users/index.treaty"));
        assert_eq!(users.children.len(), 1);

        let id = &users.children[0];
        assert_eq!(id.dir_path, "routes/users/[id]");
        assert!(id.is_dynamic);
        assert!(id.param_name.as_deref() == Some("id"));
        assert_eq!(id.segment, "[id]");
        assert_eq!(id.index_file.as_deref(), Some("routes/users/[id]/index.treaty"));
    }

    #[test]
    fn dynamic_page_file_segment() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes")])
            .with("routes", vec![Entry::file("[id].ts")]);
        let root = scan_routes(&c, &tree).unwrap();
        assert_eq!(root.page_files.len(), 1);
        let p = &root.page_files[0];
        assert!(p.is_dynamic);
        assert_eq!(p.param_name.as_deref(), Some("id"));
        assert_eq!(p.segment, "[id]");
    }

    #[test]
    fn route_group_directory_keeps_paren_segment() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes")])
            .with("routes", vec![Entry::dir("(marketing)")])
            .with("routes/(marketing)", vec![Entry::file("index.treaty")]);
        let root = scan_routes(&c, &tree).unwrap();
        let group = &root.children[0];
        assert_eq!(group.segment, "(marketing)");
        assert!(!group.is_dynamic);
        assert!(group.param_name.is_none());
        assert_eq!(
            group.index_file.as_deref(),
            Some("routes/(marketing)/index.treaty")
        );
    }

    // --- scan_api: nesting + dynamic ----------------------------------------

    #[test]
    fn api_nesting_index_and_dynamic() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("api")])
            .with(
                "api",
                vec![Entry::file("index.ts"), Entry::dir("users")],
            )
            .with(
                "api/users",
                vec![Entry::file("index.ts"), Entry::dir("[id]")],
            )
            .with("api/users/[id]", vec![Entry::file("index.ts")]);

        let root = scan_api(&c, &tree).unwrap();
        assert_eq!(root.dir_path, "api");
        assert_eq!(root.handler_files.len(), 1);
        assert!(root.handler_files[0].is_index);
        assert_eq!(root.handler_files[0].segment, "");

        let users = &root.children[0];
        assert_eq!(users.segment, "users");
        assert_eq!(users.handler_files.len(), 1);
        assert!(users.handler_files[0].is_index);

        let id = &users.children[0];
        assert!(id.is_dynamic);
        assert_eq!(id.param_name.as_deref(), Some("id"));
        assert_eq!(id.segment, "[id]");
        assert_eq!(id.handler_files.len(), 1);
        assert!(id.handler_files[0].is_index);
    }

    #[test]
    fn api_non_index_handler_segment() {
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("api")])
            .with(
                "api",
                vec![Entry::file("health.ts"), Entry::file("[token].ts")],
            );
        let root = scan_api(&c, &tree).unwrap();
        // Sorted: "[token].ts" before "health.ts".
        assert_eq!(root.handler_files.len(), 2);
        let token = &root.handler_files[0];
        assert_eq!(token.file_path, "api/[token].ts");
        assert!(token.is_dynamic);
        assert!(!token.is_index);
        assert_eq!(token.param_name.as_deref(), Some("token"));
        assert_eq!(token.segment, "[token]");

        let health = &root.handler_files[1];
        assert_eq!(health.segment, "health");
        assert!(!health.is_dynamic);
        assert!(!health.is_index);
    }

    #[test]
    fn api_extensions_differ_from_route_extensions() {
        // api default extensions are [".treaty", ".ts"] — a ".tsx" file is a
        // route file but NOT an api handler.
        let c = cfg();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("api")])
            .with(
                "api",
                vec![Entry::file("a.tsx"), Entry::file("b.ts")],
            );
        let root = scan_api(&c, &tree).unwrap();
        assert_eq!(root.handler_files.len(), 1);
        assert_eq!(root.handler_files[0].file_path, "api/b.ts");
    }

    // --- configurable directory names / root_dir ---------------------------

    #[test]
    fn custom_dir_names_and_root_dir() {
        let c = FileRoutingConfig::resolve(PartialFileRoutingConfig {
            root_dir: Some("src/app".to_string()),
            routes_dir: Some("pages".to_string()),
            api_dir: Some("server".to_string()),
            ..Default::default()
        });
        let tree = MemTree::default()
            .with("src/app", vec![Entry::dir("pages"), Entry::dir("server")])
            .with("src/app/pages", vec![Entry::file("index.treaty")])
            .with("src/app/server", vec![Entry::file("ping.ts")]);

        let routes = scan_routes(&c, &tree).unwrap();
        assert_eq!(routes.dir_path, "src/app/pages");
        assert_eq!(
            routes.index_file.as_deref(),
            Some("src/app/pages/index.treaty")
        );

        let api = scan_api(&c, &tree).unwrap();
        assert_eq!(api.dir_path, "src/app/server");
        assert_eq!(api.handler_files[0].file_path, "src/app/server/ping.ts");
    }

    #[test]
    fn children_are_sorted_deterministically() {
        let c = cfg();
        // Provide entries out of order; expect lexicographic sort on output.
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes")])
            .with(
                "routes",
                vec![Entry::dir("zebra"), Entry::dir("alpha"), Entry::dir("mango")],
            )
            .with("routes/zebra", vec![])
            .with("routes/alpha", vec![])
            .with("routes/mango", vec![]);
        let root = scan_routes(&c, &tree).unwrap();
        let names: Vec<_> = root.children.iter().map(|c| c.segment.clone()).collect();
        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }
}
