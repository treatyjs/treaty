//! End-to-end integration test: run [`generate_routing`] against the REAL
//! `examples/file-routed-app` directory tree on disk, via [`RealFsDirTree`], and
//! assert the produced Angular routes, Federation remotes, and API endpoints
//! match the verified expected tables documented in that app's `README.md`.
//!
//! This is the only test in the crate that touches the actual filesystem through
//! the production [`RealFsDirTree`] adapter (the unit tests use in-memory
//! `MemTree` fixtures). It proves the whole pipeline — `RealFsDirTree` ->
//! `scan_routes` / `scan_api` -> route/remote/endpoint lowering — works against a
//! genuine directory, not just fixtures.
//!
//! The example lives at `<repo>/examples/file-routed-app`. We resolve it from
//! `CARGO_MANIFEST_DIR` (which points at `libs/file-routing`) by walking up two
//! levels to the repo root, so the test is location-independent.

use std::path::PathBuf;

use treaty_file_routing::{
    generate_routing, AngularRoute, DynamicSegmentStyle, FileRoutingConfig, GeneratedRouting,
    PartialFileRoutingConfig, RealFsDirTree,
};

/// Absolute path to the bundled example app, resolved from the crate manifest
/// dir (`libs/file-routing`) up to the repo root and into `examples/`.
fn example_app_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .join("..")
        .join("..")
        .canonicalize()
        .expect("canonicalize repo root from CARGO_MANIFEST_DIR/../..");
    let app = root.join("examples").join("file-routed-app");
    assert!(
        app.join("routes").is_dir() && app.join("api").is_dir(),
        "expected example app with routes/ and api/ at {}",
        app.display()
    );
    app
}

/// Flatten a route tree into `(depth, path, kind, file)` tuples in emission
/// order, where `kind` is `"layout"` when the route carries a layout file else
/// `"leaf"`, and `file` is the component file or, for a structural layout route,
/// the layout file. This mirrors the README's indented route table.
fn flatten_routes(routes: &[AngularRoute], depth: usize, out: &mut Vec<(usize, String, &'static str, String)>) {
    for r in routes {
        let kind = if r.layout_file.is_some() { "layout" } else { "leaf" };
        let file = r
            .component_file
            .clone()
            .or_else(|| r.layout_file.clone())
            .unwrap_or_else(|| "(structural)".to_string());
        out.push((depth, r.path.clone(), kind, file));
        flatten_routes(&r.children, depth + 1, out);
    }
}

/// Recursively assert no route in the tree carries a federation `remote_name`.
fn assert_no_remote_names(routes: &[AngularRoute]) {
    for r in routes {
        assert!(
            r.remote_name.is_none(),
            "route {:?} should have no remote_name when federation is off",
            r.path
        );
        assert_no_remote_names(&r.children);
    }
}

#[test]
fn default_config_matches_readme_tables() {
    let tree = RealFsDirTree::new(example_app_dir());
    let out: GeneratedRouting = generate_routing(&FileRoutingConfig::default(), &tree);

    // --- Angular routes ----------------------------------------------------
    // Single root-layout parent wrapping everything; depth/order mirror the
    // README's "Angular routes" table exactly.
    assert_eq!(out.routes.len(), 1, "one top-level root-layout route");

    let mut flat = Vec::new();
    flatten_routes(&out.routes, 0, &mut flat);

    let expected: Vec<(usize, &str, &str, &str)> = vec![
        (0, "", "layout", "routes/layout.treaty"),
        (1, "", "leaf", "routes/index.treaty"),
        (1, "(marketing)", "leaf", "routes/(marketing)/index.treaty"),
        (1, "(marketing)/about", "leaf", "routes/(marketing)/about.tjsx"),
        (1, "blog", "layout", "routes/blog/layout.treaty"),
        (2, "", "leaf", "routes/blog/index.treaty"),
        (2, "[...path]", "leaf", "routes/blog/[...path]/index.treaty"),
        (2, "[slug]", "leaf", "routes/blog/[slug]/index.treaty"),
        (1, "docs/[category]/[page]", "leaf", "routes/docs/[category]/[page]/index.tjsx"),
        (1, "**", "leaf", "routes/not-found.treaty"),
    ];
    let actual: Vec<(usize, &str, &str, &str)> = flat
        .iter()
        .map(|(d, p, k, f)| (*d, p.as_str(), *k, f.as_str()))
        .collect();
    assert_eq!(actual, expected, "Angular route table");

    // The wildcard route is flagged.
    let wildcard_count = out
        .routes
        .iter()
        .flat_map(|r| std::iter::once(r).chain(descendants(r)))
        .filter(|r| r.is_wildcard)
        .count();
    assert_eq!(wildcard_count, 1, "exactly one '**' wildcard route");

    // --- Federation remotes (default: on) ----------------------------------
    // One remote per lazy route + per layout boundary, depth-first route order.
    let remote_rows: Vec<(&str, &str, &str)> = out
        .remotes
        .iter()
        .map(|r| (r.name.as_str(), r.route_path.as_str(), r.entry_file.as_str()))
        .collect();
    let expected_remotes: Vec<(&str, &str, &str)> = vec![
        ("root", "", "routes/layout.treaty"),
        ("root", "", "routes/index.treaty"),
        ("marketing", "(marketing)", "routes/(marketing)/index.treaty"),
        ("marketing-about", "(marketing)/about", "routes/(marketing)/about.tjsx"),
        ("blog", "blog", "routes/blog/layout.treaty"),
        ("root", "", "routes/blog/index.treaty"),
        ("path", "[...path]", "routes/blog/[...path]/index.treaty"),
        ("slug", "[slug]", "routes/blog/[slug]/index.treaty"),
        ("docs-category-page", "docs/[category]/[page]", "routes/docs/[category]/[page]/index.tjsx"),
        ("not-found", "**", "routes/not-found.treaty"),
    ];
    assert_eq!(remote_rows, expected_remotes, "Federation remote table");
    assert!(
        out.remotes.iter().all(|r| r.exposed_module == "./Route"),
        "every remote exposes ./Route"
    );
    // There is exactly one remote per emitted route (10 routes, 10 remotes).
    assert_eq!(out.remotes.len(), flat.len(), "one remote per emitted route");

    // --- API endpoints -----------------------------------------------------
    // Sorted by path; api dynamic segments always render :param.
    let ep_rows: Vec<(&str, &str, Vec<&str>)> = out
        .endpoints
        .iter()
        .map(|e| {
            (
                e.path.as_str(),
                e.handler_file.as_str(),
                e.param_names.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
        .collect();
    let expected_eps: Vec<(&str, &str, Vec<&str>)> = vec![
        ("/", "api/index.ts", vec![]),
        ("/health", "api/health/index.ts", vec![]),
        ("/posts", "api/posts/index.ts", vec![]),
        ("/posts/:id", "api/posts/[id]/index.ts", vec!["id"]),
    ];
    assert_eq!(ep_rows, expected_eps, "API endpoint table");

    // Whole output round-trips through serde losslessly.
    let json = serde_json::to_string(&out).unwrap();
    let back: GeneratedRouting = serde_json::from_str(&json).unwrap();
    assert_eq!(out, back, "GeneratedRouting serde round-trip");
}

#[test]
fn colon_style_and_federation_off_overrides_against_real_dir() {
    // Same real directory, but: dynamic segments rendered :param (Colon style)
    // and federation disabled. Proves config overrides thread end-to-end from
    // the real-FS scan through route/remote/endpoint lowering.
    let config = FileRoutingConfig::resolve(PartialFileRoutingConfig {
        dynamic_segment_style: Some(DynamicSegmentStyle::Colon),
        federation: Some(false),
        ..Default::default()
    });
    let tree = RealFsDirTree::new(example_app_dir());
    let out = generate_routing(&config, &tree);

    // --- Routes: Colon style flips bracket dynamic segments to :param ------
    let mut flat = Vec::new();
    flatten_routes(&out.routes, 0, &mut flat);
    let paths: Vec<&str> = flat.iter().map(|(_, p, _, _)| p.as_str()).collect();
    let expected_paths = vec![
        "",
        "",
        "(marketing)",
        "(marketing)/about",
        "blog",
        "",
        ":...path",
        ":slug",
        "docs/:category/:page",
        "**",
    ];
    assert_eq!(paths, expected_paths, "route paths under Colon style");
    assert!(
        flat.iter().all(|(_, p, _, _)| !p.contains('[')),
        "no bracket segments survive under Colon style"
    );

    // Component/layout files are unaffected by the style (they are file paths,
    // not URL segments): the catch-all and dynamic dirs are still on disk as
    // bracket directories.
    let slug = flat.iter().find(|(_, p, _, _)| p == ":slug").unwrap();
    assert_eq!(slug.3, "routes/blog/[slug]/index.treaty");
    let catch = flat.iter().find(|(_, p, _, _)| p == ":...path").unwrap();
    assert_eq!(catch.3, "routes/blog/[...path]/index.treaty");

    // --- Federation OFF: no remotes, no remote_name anywhere ---------------
    assert!(out.remotes.is_empty(), "federation off yields zero remotes");
    assert_no_remote_names(&out.routes);

    // --- API endpoints: identical paths (api always colonises) -------------
    let ep_rows: Vec<(&str, &str, Vec<&str>)> = out
        .endpoints
        .iter()
        .map(|e| {
            (
                e.path.as_str(),
                e.handler_file.as_str(),
                e.param_names.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        ep_rows,
        vec![
            ("/", "api/index.ts", vec![]),
            ("/health", "api/health/index.ts", vec![]),
            ("/posts", "api/posts/index.ts", vec![]),
            ("/posts/:id", "api/posts/[id]/index.ts", vec!["id"]),
        ],
        "API endpoints are independent of dynamic_segment_style"
    );
}

/// Yield every descendant route of `r` (depth-first), excluding `r` itself.
fn descendants(r: &AngularRoute) -> Box<dyn Iterator<Item = &AngularRoute> + '_> {
    Box::new(
        r.children
            .iter()
            .flat_map(|c| std::iter::once(c).chain(descendants(c))),
    )
}
