//! Stage 2: lower a scanned [`RouteNode`] tree into Angular lazy routes and,
//! when federation is enabled, Module Federation remote descriptors.
//!
//! This applies Angular routing semantics: index files become `path: ""`
//! routes, layouts become parent routes wrapping `children`, not-found files
//! become `path: "**"` wildcards, and dynamic segments are emitted as `:param`.
//!
//! # Lowering model
//!
//! Each [`RouteNode`] is a directory. The directory contributes its `segment`
//! (already normalised by the scanner — `""` for the routes root, a static name
//! for a plain dir, or `:param` for a dynamic dir) as a URL path prefix for
//! everything inside it.
//!
//! * **Layout dir** — when a node has a `layout_file`, it becomes a single
//!   parent [`AngularRoute`] at the node's `segment`, with the layout as its
//!   `layout_file`. The index file, page files, child directories, and a
//!   not-found file are lowered as that route's `children` (their paths made
//!   relative to the layout). This is Angular's parent/children nesting.
//! * **Non-layout dir** — without a layout there is nothing to nest under, so
//!   the node's contents are *flattened* into the parent's route list with the
//!   directory `segment` prefixed onto each emitted path.
//! * **Index file** — the directory's own route. Under a layout it is the
//!   empty-path (`""`) child; flattened it takes the directory's joined path.
//! * **Page files** — sibling leaf routes; each page segment is appended to the
//!   directory path.
//! * **Not-found file** — emitted as a `path: "**"` wildcard route. At the root
//!   it is a top-level catch-all; inside a layout it is a scoped catch-all child.

use crate::config::{AngularRoute, FederationRemote, FileRoutingConfig, RouteNode};

/// Lower the scanned routes tree into top-level Angular routes. An absent root
/// (`None`) yields an empty `Vec`.
///
/// The root [`RouteNode`] (the `routes/` directory itself) has an empty
/// `segment`, so its contents land directly at the top level. A root with a
/// layout still nests its contents under that layout's `""` parent route.
pub fn build_routes(config: &FileRoutingConfig, root: Option<&RouteNode>) -> Vec<AngularRoute> {
    match root {
        Some(node) => lower_node(config, node, ""),
        None => Vec::new(),
    }
}

/// Lower a single [`RouteNode`] into the routes it contributes to its parent's
/// child list. `prefix` is the URL path accumulated from ancestor directories
/// that have *not* yet been materialised as a parent route (i.e. flattened
/// non-layout ancestors); it is joined ahead of this node's own `segment`.
fn lower_node(config: &FileRoutingConfig, node: &RouteNode, prefix: &str) -> Vec<AngularRoute> {
    let dir_path = join_path(prefix, &node.segment);

    if node.layout_file.is_some() {
        // Layout present: this directory becomes one parent route at `dir_path`
        // and everything inside it nests as children with paths relative to the
        // layout (so the children carry an empty prefix of their own).
        let mut children = Vec::new();

        if let Some(index_file) = &node.index_file {
            children.push(leaf_route(config, String::new(), index_file, ""));
        }
        for page in &node.page_files {
            children.push(leaf_route(config, page.segment.clone(), &page.file_path, ""));
        }
        for child in &node.children {
            children.extend(lower_node(config, child, ""));
        }
        if let Some(nf) = &node.not_found_file {
            children.push(wildcard_route(config, nf, ""));
        }

        let mut parent = AngularRoute {
            path: dir_path.clone(),
            component_file: None,
            layout_file: node.layout_file.clone(),
            is_wildcard: false,
            remote_name: None,
            children,
        };
        // A layout parent is itself a federation boundary: its subtree loads as
        // one remote, keyed on the path it mounts at.
        if config.federation {
            parent.remote_name = Some(remote_name_for(&parent.path));
        }
        vec![parent]
    } else {
        // No layout: flatten this directory's contents into the parent's list,
        // carrying `dir_path` forward as the prefix for everything inside.
        let mut out = Vec::new();

        if let Some(index_file) = &node.index_file {
            out.push(leaf_route(config, dir_path.clone(), index_file, ""));
        }
        for page in &node.page_files {
            let path = join_path(&dir_path, &page.segment);
            out.push(leaf_route(config, path, &page.file_path, ""));
        }
        for child in &node.children {
            out.extend(lower_node(config, child, &dir_path));
        }
        if let Some(nf) = &node.not_found_file {
            out.push(wildcard_route(config, nf, &dir_path));
        }

        out
    }
}

/// Build a lazily-loaded leaf [`AngularRoute`] for a component file at `path`.
/// `_unused_prefix` keeps the call sites symmetric with [`wildcard_route`].
fn leaf_route(
    config: &FileRoutingConfig,
    path: String,
    component_file: &str,
    _unused_prefix: &str,
) -> AngularRoute {
    let remote_name = if config.federation {
        Some(remote_name_for(&path))
    } else {
        None
    };
    AngularRoute {
        path,
        component_file: Some(component_file.to_string()),
        layout_file: None,
        is_wildcard: false,
        remote_name,
        children: Vec::new(),
    }
}

/// Build the `path: "**"` wildcard route for a not-found file. The wildcard path
/// is always literally `"**"`; `prefix` only documents the directory scope (a
/// scoped not-found inside a layout vs. a top-level one) and does not alter the
/// emitted path, matching Angular's relative-wildcard semantics.
fn wildcard_route(config: &FileRoutingConfig, not_found_file: &str, _prefix: &str) -> AngularRoute {
    let remote_name = if config.federation {
        Some(remote_name_for("**"))
    } else {
        None
    };
    AngularRoute {
        path: "**".to_string(),
        component_file: Some(not_found_file.to_string()),
        layout_file: None,
        is_wildcard: true,
        remote_name,
        children: Vec::new(),
    }
}

/// Join two URL path fragments with a single `/`, dropping empties so the root
/// (`""`) never introduces a leading slash and adjacent slashes never double up.
fn join_path(prefix: &str, segment: &str) -> String {
    match (prefix.is_empty(), segment.is_empty()) {
        (true, _) => segment.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{segment}"),
    }
}

/// Derive the full set of Module Federation remote descriptors from the built
/// route tree. Returns an empty `Vec` when `config.federation` is `false`.
///
/// Every route that carries a `remote_name` (i.e. every lazy component route and
/// every layout boundary) yields one [`FederationRemote`], realising the
/// route-as-remote model: each lazy route is independently deployable. The tree
/// is walked depth-first so remotes are produced in stable, route-order.
pub fn build_remotes(config: &FileRoutingConfig, routes: &[AngularRoute]) -> Vec<FederationRemote> {
    if !config.federation {
        return Vec::new();
    }
    let mut out = Vec::new();
    collect_remotes(routes, &mut out);
    out
}

/// Depth-first walk appending a [`FederationRemote`] for each route bearing a
/// `remote_name`. A route's entry file is its `component_file`, or its
/// `layout_file` for a structural layout boundary.
fn collect_remotes(routes: &[AngularRoute], out: &mut Vec<FederationRemote>) {
    for route in routes {
        if let Some(name) = &route.remote_name
            && let Some(entry_file) = route.component_file.as_ref().or(route.layout_file.as_ref())
        {
            out.push(FederationRemote {
                name: name.clone(),
                exposed_module: "./Route".to_string(),
                entry_file: entry_file.clone(),
                route_path: route.path.clone(),
            });
        }
        collect_remotes(&route.children, out);
    }
}

/// Slugify a route path into a stable Module Federation remote name.
///
/// The empty (index) path becomes `"root"`; the `"**"` wildcard becomes
/// `"not-found"`. Otherwise each path segment is lowercased, dynamic markers
/// (`:`) are dropped, and non-alphanumeric runs collapse to single `-`, joined
/// by `-`. Deterministic so a given path always maps to the same remote.
fn remote_name_for(path: &str) -> String {
    if path.is_empty() {
        return "root".to_string();
    }
    if path == "**" {
        return "not-found".to_string();
    }
    let mut name = String::new();
    let mut prev_dash = false;
    for ch in path.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !name.is_empty() {
            // Collapse any separator run (`/`, `:`, etc.) to a single dash.
            name.push('-');
            prev_dash = true;
        }
    }
    // Trim a trailing dash that a path ending in a separator could leave behind.
    if name.ends_with('-') {
        name.pop();
    }
    if name.is_empty() {
        "root".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoutePage;

    /// Convenience: a static (non-dynamic) directory node.
    fn dir_node(dir_path: &str, segment: &str) -> RouteNode {
        RouteNode {
            dir_path: dir_path.to_string(),
            segment: segment.to_string(),
            is_dynamic: false,
            param_name: None,
            index_file: None,
            layout_file: None,
            not_found_file: None,
            page_files: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Convenience: a dynamic directory node whose `segment` is already the
    /// scanner-normalised `:param` form.
    fn dynamic_dir_node(dir_path: &str, param: &str) -> RouteNode {
        RouteNode {
            dir_path: dir_path.to_string(),
            segment: format!(":{param}"),
            is_dynamic: true,
            param_name: Some(param.to_string()),
            index_file: None,
            layout_file: None,
            not_found_file: None,
            page_files: Vec::new(),
            children: Vec::new(),
        }
    }

    fn page(file_path: &str, segment: &str) -> RoutePage {
        RoutePage {
            file_path: file_path.to_string(),
            segment: segment.to_string(),
            is_dynamic: false,
            param_name: None,
        }
    }

    /// Federation off everywhere by default in these tests unless asked.
    fn no_fed() -> FileRoutingConfig {
        FileRoutingConfig {
            federation: false,
            ..Default::default()
        }
    }

    #[test]
    fn absent_root_yields_no_routes() {
        let c = no_fed();
        assert!(build_routes(&c, None).is_empty());
    }

    #[test]
    fn root_index_becomes_empty_path_route() {
        let c = no_fed();
        let mut root = dir_node("routes", "");
        root.index_file = Some("routes/index.treaty".to_string());

        let routes = build_routes(&c, Some(&root));
        assert_eq!(routes.len(), 1);
        let r = &routes[0];
        assert_eq!(r.path, "");
        assert_eq!(r.component_file.as_deref(), Some("routes/index.treaty"));
        assert!(!r.is_wildcard);
        assert!(r.layout_file.is_none());
        assert!(r.remote_name.is_none());
        assert!(r.children.is_empty());
    }

    #[test]
    fn static_pages_get_directory_prefixed_paths() {
        let c = no_fed();
        // routes/ with a top-level "about" page and a nested "users" dir holding
        // an index + a "profile" page.
        let mut root = dir_node("routes", "");
        root.page_files = vec![page("routes/about.treaty", "about")];

        let mut users = dir_node("routes/users", "users");
        users.index_file = Some("routes/users/index.treaty".to_string());
        users.page_files = vec![page("routes/users/profile.treaty", "profile")];
        root.children = vec![users];

        let routes = build_routes(&c, Some(&root));
        // Flattened (no layouts): about, users (index), users/profile.
        let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["about", "users", "users/profile"]);

        let users_index = routes.iter().find(|r| r.path == "users").unwrap();
        assert_eq!(
            users_index.component_file.as_deref(),
            Some("routes/users/index.treaty")
        );
        let profile = routes.iter().find(|r| r.path == "users/profile").unwrap();
        assert_eq!(
            profile.component_file.as_deref(),
            Some("routes/users/profile.treaty")
        );
    }

    #[test]
    fn dynamic_segment_emits_param_path() {
        let c = no_fed();
        // routes/users/[id]/index.treaty  ->  users/:id
        let mut root = dir_node("routes", "");
        let mut users = dir_node("routes/users", "users");
        let mut id = dynamic_dir_node("routes/users/:id", "id");
        id.index_file = Some("routes/users/:id/index.treaty".to_string());
        users.children = vec![id];
        root.children = vec![users];

        let routes = build_routes(&c, Some(&root));
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "users/:id");
        assert_eq!(
            routes[0].component_file.as_deref(),
            Some("routes/users/:id/index.treaty")
        );
    }

    #[test]
    fn not_found_lowers_to_wildcard() {
        let c = no_fed();
        let mut root = dir_node("routes", "");
        root.index_file = Some("routes/index.treaty".to_string());
        root.not_found_file = Some("routes/not-found.treaty".to_string());

        let routes = build_routes(&c, Some(&root));
        let wild = routes.iter().find(|r| r.is_wildcard).unwrap();
        assert_eq!(wild.path, "**");
        assert_eq!(
            wild.component_file.as_deref(),
            Some("routes/not-found.treaty")
        );
        // The index route is still present and not a wildcard.
        assert!(routes.iter().any(|r| r.path.is_empty() && !r.is_wildcard));
    }

    #[test]
    fn layout_becomes_parent_with_nested_children() {
        let c = no_fed();
        // routes/
        //   layout.treaty            (root layout)
        //   index.treaty             (root index -> "" child)
        //   dashboard/
        //     layout.treaty          (nested layout)
        //     index.treaty           ("" under dashboard)
        //     [id]/index.treaty      (":id" under dashboard)
        //     not-found.treaty       ("**" scoped under dashboard)
        let mut root = dir_node("routes", "");
        root.layout_file = Some("routes/layout.treaty".to_string());
        root.index_file = Some("routes/index.treaty".to_string());

        let mut dash = dir_node("routes/dashboard", "dashboard");
        dash.layout_file = Some("routes/dashboard/layout.treaty".to_string());
        dash.index_file = Some("routes/dashboard/index.treaty".to_string());
        dash.not_found_file = Some("routes/dashboard/not-found.treaty".to_string());

        let mut id = dynamic_dir_node("routes/dashboard/:id", "id");
        id.index_file = Some("routes/dashboard/:id/index.treaty".to_string());
        dash.children = vec![id];

        root.children = vec![dash];

        let routes = build_routes(&c, Some(&root));

        // Root layout is one parent route at path "".
        assert_eq!(routes.len(), 1);
        let root_route = &routes[0];
        assert_eq!(root_route.path, "");
        assert_eq!(
            root_route.layout_file.as_deref(),
            Some("routes/layout.treaty")
        );
        assert!(root_route.component_file.is_none());

        // Root layout children: the index ("") and the dashboard layout parent.
        let root_index = root_route
            .children
            .iter()
            .find(|r| r.path.is_empty() && !r.is_wildcard)
            .unwrap();
        assert_eq!(
            root_index.component_file.as_deref(),
            Some("routes/index.treaty")
        );

        let dash_route = root_route
            .children
            .iter()
            .find(|r| r.path == "dashboard")
            .unwrap();
        assert_eq!(
            dash_route.layout_file.as_deref(),
            Some("routes/dashboard/layout.treaty")
        );

        // Dashboard children: its index (""), the dynamic ":id", and a scoped "**".
        let dash_index = dash_route
            .children
            .iter()
            .find(|r| r.path.is_empty() && !r.is_wildcard)
            .unwrap();
        assert_eq!(
            dash_index.component_file.as_deref(),
            Some("routes/dashboard/index.treaty")
        );
        let dash_id = dash_route.children.iter().find(|r| r.path == ":id").unwrap();
        assert_eq!(
            dash_id.component_file.as_deref(),
            Some("routes/dashboard/:id/index.treaty")
        );
        let dash_wild = dash_route.children.iter().find(|r| r.is_wildcard).unwrap();
        assert_eq!(dash_wild.path, "**");
        assert_eq!(
            dash_wild.component_file.as_deref(),
            Some("routes/dashboard/not-found.treaty")
        );
    }

    #[test]
    fn federation_off_yields_no_remotes_and_no_remote_names() {
        let c = no_fed();
        let mut root = dir_node("routes", "");
        root.index_file = Some("routes/index.treaty".to_string());
        root.page_files = vec![page("routes/about.treaty", "about")];

        let routes = build_routes(&c, Some(&root));
        assert!(routes.iter().all(|r| r.remote_name.is_none()));
        assert!(build_remotes(&c, &routes).is_empty());
    }

    #[test]
    fn federation_on_emits_one_remote_per_lazy_route() {
        let c = FileRoutingConfig::default(); // federation = true
        // routes/
        //   index.treaty             -> remote "root"
        //   about.treaty             -> remote "about"
        //   users/[id]/index.treaty  -> remote "users-id"
        let mut root = dir_node("routes", "");
        root.index_file = Some("routes/index.treaty".to_string());
        root.page_files = vec![page("routes/about.treaty", "about")];

        let mut users = dir_node("routes/users", "users");
        let mut id = dynamic_dir_node("routes/users/:id", "id");
        id.index_file = Some("routes/users/:id/index.treaty".to_string());
        users.children = vec![id];
        root.children = vec![users];

        let routes = build_routes(&c, Some(&root));
        // Every emitted lazy route carries a remote name.
        assert!(routes.iter().all(|r| r.remote_name.is_some()));

        let remotes = build_remotes(&c, &routes);
        let names: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["root", "about", "users-id"]);

        let users_remote = remotes.iter().find(|r| r.name == "users-id").unwrap();
        assert_eq!(users_remote.route_path, "users/:id");
        assert_eq!(users_remote.exposed_module, "./Route");
        assert_eq!(users_remote.entry_file, "routes/users/:id/index.treaty");

        let root_remote = remotes.iter().find(|r| r.name == "root").unwrap();
        assert_eq!(root_remote.route_path, "");
        assert_eq!(root_remote.entry_file, "routes/index.treaty");
    }

    #[test]
    fn federation_on_emits_remote_for_layout_boundary_and_children() {
        let c = FileRoutingConfig::default();
        let mut root = dir_node("routes", "");
        root.layout_file = Some("routes/layout.treaty".to_string());
        root.index_file = Some("routes/index.treaty".to_string());

        let mut dash = dir_node("routes/dashboard", "dashboard");
        dash.index_file = Some("routes/dashboard/index.treaty".to_string());
        root.children = vec![dash];

        let routes = build_routes(&c, Some(&root));
        let remotes = build_remotes(&c, &routes);

        // Layout boundary itself is a remote (entry = the layout file), keyed
        // on its path -> "root", plus the nested index and dashboard routes.
        let names: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
        // Depth-first: layout parent ("root"), then its "" index child also
        // slugifies to "root", then "dashboard".
        assert_eq!(names, vec!["root", "root", "dashboard"]);

        let layout_remote = remotes.iter().find(|r| r.route_path.is_empty()).unwrap();
        assert_eq!(layout_remote.entry_file, "routes/layout.treaty");

        let dash_remote = remotes.iter().find(|r| r.route_path == "dashboard").unwrap();
        assert_eq!(dash_remote.entry_file, "routes/dashboard/index.treaty");
    }

    #[test]
    fn wildcard_at_root_is_top_level() {
        let c = no_fed();
        let mut root = dir_node("routes", "");
        root.not_found_file = Some("routes/not-found.treaty".to_string());
        let routes = build_routes(&c, Some(&root));
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "**");
        assert!(routes[0].is_wildcard);
    }
}
