//! `treaty_file_routing` — the pure, deterministic Rust core for Treaty's
//! file-based routing.
//!
//! Treaty is a compiler, not a host. This crate turns a *directory tree* plus a
//! *config* into *data*: an Angular lazy-route config, Module Federation remote
//! descriptors, and a server-endpoint manifest. It performs no I/O of its own —
//! the directory tree is supplied through the injectable [`DirTree`] trait, so
//! the core logic is unit-tested with in-memory fixtures and a real-filesystem
//! `DirTree` (or a NAPI-fed one) can be dropped in later. See
//! [[rust-core-ts-shim-layering]].
//!
//! Pipeline: [`scanner`] walks the tree into raw [`RouteNode`]/[`ApiNode`]
//! trees; [`routes`] lowers routes into [`AngularRoute`]s plus
//! [`FederationRemote`]s; [`api`] lowers the api tree into [`ApiEndpoint`]s.
//! [`generate_routing`] wires the three into a [`GeneratedRouting`].

pub mod api;
pub mod config;
pub mod routes;
pub mod scanner;

pub use config::{
    AngularRoute, ApiEndpoint, ApiHandlerFile, ApiNode, DirTree, DynamicSegmentStyle, Entry,
    EntryKind, FederationRemote, FileRoutingConfig, GeneratedRouting, PartialFileRoutingConfig,
    RouteNode, RoutePage,
};

/// Run the full file-routing pipeline against `tree` under `config`.
///
/// When `config.enabled` is `false`, returns an empty [`GeneratedRouting`]
/// without scanning. Otherwise scans the `routes/` and `api/` directories,
/// lowers them, and (when `config.federation`) derives remotes.
pub fn generate_routing(config: &FileRoutingConfig, tree: &dyn DirTree) -> GeneratedRouting {
    if !config.enabled {
        return GeneratedRouting::default();
    }

    let route_root = scanner::scan_routes(config, tree);
    let api_root = scanner::scan_api(config, tree);

    let routes = routes::build_routes(config, route_root.as_ref());
    let remotes = if config.federation {
        routes::build_remotes(config, &routes)
    } else {
        Vec::new()
    };
    let endpoints = api::build_endpoints(config, api_root.as_ref());

    GeneratedRouting {
        routes,
        remotes,
        endpoints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn default_config_matches_conventions() {
        let c = FileRoutingConfig::default();
        assert!(c.enabled);
        assert_eq!(c.routes_dir, "routes");
        assert_eq!(c.api_dir, "api");
        assert_eq!(c.layout_file_name, "layout");
        assert_eq!(c.not_found_file_name, "not-found");
        assert_eq!(c.index_file_names, vec!["index", "page"]);
        assert_eq!(
            c.route_extensions,
            vec![".treaty", ".tjsx", ".tsx", ".ts"]
        );
        assert_eq!(c.dynamic_segment_style, DynamicSegmentStyle::Bracket);
        assert!(c.federation);
    }

    #[test]
    fn resolve_overlays_only_supplied_fields() {
        let partial = PartialFileRoutingConfig {
            routes_dir: Some("pages".to_string()),
            federation: Some(false),
            dynamic_segment_style: Some(DynamicSegmentStyle::Colon),
            ..Default::default()
        };
        let c = FileRoutingConfig::resolve(partial);
        assert_eq!(c.routes_dir, "pages");
        assert!(!c.federation);
        assert_eq!(c.dynamic_segment_style, DynamicSegmentStyle::Colon);
        // Untouched fields keep defaults.
        assert_eq!(c.api_dir, "api");
        assert!(c.enabled);
    }

    #[test]
    fn match_route_extension_prefers_longest() {
        let c = FileRoutingConfig::default();
        assert_eq!(
            c.match_route_extension("index.treaty"),
            Some(("index", ".treaty".to_string()))
        );
        assert_eq!(c.match_route_extension("README.md"), None);
        // A bare extension is not a match (no stem).
        assert_eq!(c.match_route_extension(".ts"), None);
    }

    #[test]
    fn disabled_config_short_circuits() {
        let c = FileRoutingConfig {
            enabled: false,
            ..Default::default()
        };
        let tree = MemTree::default().with("", vec![Entry::dir("routes")]);
        let out = generate_routing(&c, &tree);
        assert_eq!(out, GeneratedRouting::default());
    }

    #[test]
    fn minimal_tree_serde_round_trips() {
        let c = FileRoutingConfig::default();
        let tree = MemTree::default()
            .with("", vec![Entry::dir("routes"), Entry::dir("api")])
            .with("routes", vec![Entry::file("index.treaty")]);
        let out = generate_routing(&c, &tree);
        // Wiring is sound and the full output serializes/deserializes losslessly.
        let json = serde_json::to_string(&out).unwrap();
        let back: GeneratedRouting = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }

    /// A full-fixture `routes/` (nested + dynamic + layout + not-found) and
    /// `api/` tree built in-memory and run through the *real* `generate_routing`
    /// pipeline (scan -> lower -> federation), asserting the produced
    /// `AngularRoutes`, `FederationRemotes`, and `ApiEndpoints` end to end with
    /// the default conventions (Bracket dynamic style, federation on).
    fn full_fixture() -> MemTree {
        // routes/
        //   layout.treaty                 (root layout -> parent route at "")
        //   index.treaty                  (root index  -> "" child)
        //   about.treaty                  (page        -> "about" child)
        //   not-found.treaty              (404         -> "**" child)
        //   users/
        //     index.treaty                (users index)
        //     [id]/index.treaty           (dynamic users/:id)
        // api/
        //   index.ts                      ( / , all methods)
        //   health.get.ts                 (/health.get, GET — segment is the
        //                                   full stem; method inference is a
        //                                   separate concern from the path)
        //   users/
        //     index.ts                     (/users, all methods)
        //     [id]/index.ts                (/users/:id)
        MemTree::default()
            .with("", vec![Entry::dir("routes"), Entry::dir("api")])
            .with(
                "routes",
                vec![
                    Entry::file("layout.treaty"),
                    Entry::file("index.treaty"),
                    Entry::file("about.treaty"),
                    Entry::file("not-found.treaty"),
                    Entry::dir("users"),
                ],
            )
            .with(
                "routes/users",
                vec![Entry::file("index.treaty"), Entry::dir("[id]")],
            )
            .with("routes/users/[id]", vec![Entry::file("index.treaty")])
            .with(
                "api",
                vec![
                    Entry::file("index.ts"),
                    Entry::file("health.get.ts"),
                    Entry::dir("users"),
                ],
            )
            .with(
                "api/users",
                vec![Entry::file("index.ts"), Entry::dir("[id]")],
            )
            .with("api/users/[id]", vec![Entry::file("index.ts")])
    }

    #[test]
    fn end_to_end_default_conventions() {
        let c = FileRoutingConfig::default();
        let out = generate_routing(&c, &full_fixture());

        // --- AngularRoutes ---------------------------------------------------
        // Root layout collapses everything under a single "" parent route.
        assert_eq!(out.routes.len(), 1, "one root layout parent");
        let root = &out.routes[0];
        assert_eq!(root.path, "");
        assert_eq!(root.layout_file.as_deref(), Some("routes/layout.treaty"));
        assert!(root.component_file.is_none());

        // Children, in deterministic (sorted) emission order:
        //   "" (index), "about" (page), "users" + "users/[id]" (flattened dir),
        //   "**" (not-found).
        let child_paths: Vec<&str> = root.children.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(child_paths, vec!["", "about", "users", "users/[id]", "**"]);

        let index = root.children.iter().find(|r| r.path.is_empty()).unwrap();
        assert_eq!(index.component_file.as_deref(), Some("routes/index.treaty"));

        // Dynamic segment is the Bracket form under the default style (the route
        // builder uses the scanner segment verbatim — it is NOT colonised here).
        let user = root.children.iter().find(|r| r.path == "users/[id]").unwrap();
        assert_eq!(
            user.component_file.as_deref(),
            Some("routes/users/[id]/index.treaty")
        );

        let wild = root.children.iter().find(|r| r.is_wildcard).unwrap();
        assert_eq!(wild.path, "**");
        assert_eq!(wild.component_file.as_deref(), Some("routes/not-found.treaty"));

        // --- FederationRemotes (federation defaults on) ----------------------
        // One remote for the layout boundary + one per lowered leaf route.
        // Depth-first, route-order: layout("root"), index("root"), about,
        // users, users-id, not-found.
        let remote_names: Vec<&str> = out.remotes.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            remote_names,
            vec!["root", "root", "about", "users", "users-id", "not-found"]
        );
        let layout_remote = out
            .remotes
            .iter()
            .find(|r| r.entry_file == "routes/layout.treaty")
            .unwrap();
        assert_eq!(layout_remote.route_path, "");
        assert_eq!(layout_remote.exposed_module, "./Route");
        let users_id_remote = out.remotes.iter().find(|r| r.name == "users-id").unwrap();
        assert_eq!(users_id_remote.route_path, "users/[id]");
        assert_eq!(
            users_id_remote.entry_file,
            "routes/users/[id]/index.treaty"
        );

        // --- ApiEndpoints ----------------------------------------------------
        // Sorted by path; api dynamic segments are always emitted `:param`.
        // `health.get.ts` is a non-index handler so its segment is the full
        // stem `health.get` (path), while GET is inferred for the manifest.
        let ep_paths: Vec<&str> = out.endpoints.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(ep_paths, vec!["/", "/health.get", "/users", "/users/:id"]);
        let users_id = out.endpoints.iter().find(|e| e.path == "/users/:id").unwrap();
        assert_eq!(users_id.param_names, vec!["id".to_string()]);
        assert_eq!(users_id.handler_file, "api/users/[id]/index.ts");
        let health = out.endpoints.iter().find(|e| e.path == "/health.get").unwrap();
        assert_eq!(health.handler_file, "api/health.get.ts");

        // Whole output round-trips through serde.
        let json = serde_json::to_string(&out).unwrap();
        let back: GeneratedRouting = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }

    #[test]
    fn end_to_end_config_overrides_are_honored() {
        // Same logical tree, but under custom dir names, Colon dynamic style,
        // and federation OFF. The fixture is re-rooted at the custom dirs.
        let c = FileRoutingConfig::resolve(PartialFileRoutingConfig {
            routes_dir: Some("pages".to_string()),
            api_dir: Some("server".to_string()),
            dynamic_segment_style: Some(DynamicSegmentStyle::Colon),
            federation: Some(false),
            ..Default::default()
        });

        let tree = MemTree::default()
            .with("", vec![Entry::dir("pages"), Entry::dir("server")])
            .with(
                "pages",
                vec![
                    Entry::file("layout.treaty"),
                    Entry::file("index.treaty"),
                    Entry::dir("users"),
                ],
            )
            .with(
                "pages/users",
                vec![Entry::file("index.treaty"), Entry::dir("[id]")],
            )
            .with("pages/users/[id]", vec![Entry::file("index.treaty")])
            .with("server", vec![Entry::dir("users")])
            .with("server/users", vec![Entry::dir("[id]")])
            .with("server/users/[id]", vec![Entry::file("index.ts")]);

        let out = generate_routing(&c, &tree);

        // Custom routes_dir honored: files are scanned under pages/.
        assert_eq!(out.routes.len(), 1);
        let root = &out.routes[0];
        assert_eq!(root.layout_file.as_deref(), Some("pages/layout.treaty"));

        // Colon style honored: the dynamic route segment renders ":id" (NOT
        // "[id]"), proving the override threads scanner -> route lowering.
        let user = root.children.iter().find(|r| r.path == "users/:id").unwrap();
        assert_eq!(
            user.component_file.as_deref(),
            Some("pages/users/[id]/index.treaty")
        );
        assert!(
            root.children.iter().all(|r| !r.path.contains('[')),
            "no bracket segments should survive under Colon style"
        );

        // Federation OFF honored: no remotes, and no route carries a remote name.
        assert!(out.remotes.is_empty(), "federation off yields no remotes");
        fn assert_no_remote(routes: &[AngularRoute]) {
            for r in routes {
                assert!(r.remote_name.is_none(), "no remote_name when federation off");
                assert_no_remote(&r.children);
            }
        }
        assert_no_remote(&out.routes);

        // Custom api_dir honored: endpoints scanned under server/, dynamic param
        // still emitted ":id" (api always colonises).
        let ep_paths: Vec<&str> = out.endpoints.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(ep_paths, vec!["/users/:id"]);
        assert_eq!(out.endpoints[0].handler_file, "server/users/[id]/index.ts");
        assert_eq!(out.endpoints[0].param_names, vec!["id".to_string()]);
    }
}
