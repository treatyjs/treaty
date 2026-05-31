//! End-to-end test of the `treaty-file-routing` CLI binary itself.
//!
//! Where `example_e2e.rs` exercises the library [`generate_routing`] pipeline
//! over the real example directory, this test drives the *compiled binary* as a
//! subprocess against the same `examples/file-routed-app` tree and asserts the
//! emitted output — both the JSON manifest and the `--emit ts` route module —
//! match the known, verified route/remote/endpoint table. This proves the
//! crate is genuinely runnable end to end: real CLI -> real filesystem scan ->
//! consumable output, not a hand-transcribed artifact.
//!
//! The binary path is provided by Cargo via `CARGO_BIN_EXE_treaty-file-routing`
//! to the test process; the example app is resolved from `CARGO_MANIFEST_DIR`
//! (`libs/file-routing`) up two levels to the repo root, so the test is
//! location-independent.

use std::path::PathBuf;
use std::process::Command;

use treaty_file_routing::{
    generate_routing, FileRoutingConfig, GeneratedRouting, RealFsDirTree,
};

/// Absolute path to the compiled CLI binary under test.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_treaty-file-routing")
}

/// Absolute path to the bundled example app's project root (the dir that
/// contains `routes/` and `api/`).
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

/// Run the CLI with `args`, asserting it exited 0, and return captured stdout.
fn run_cli(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("spawn treaty-file-routing");
    assert!(
        out.status.success(),
        "CLI exited non-zero ({:?})\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("stdout is utf-8")
}

#[test]
fn cli_json_matches_library_pipeline() {
    let app = example_app_dir();
    let stdout = run_cli(&[app.to_str().unwrap()]);

    // The emitted JSON parses into the public type and is byte-identical to the
    // library pipeline run directly over the same real directory: the CLI adds
    // no semantics, it just hosts the core.
    let parsed: GeneratedRouting =
        serde_json::from_str(&stdout).expect("CLI stdout is valid GeneratedRouting JSON");
    let direct = generate_routing(&FileRoutingConfig::default(), &RealFsDirTree::new(&app));
    assert_eq!(parsed, direct, "CLI JSON equals the library pipeline output");

    // Default config -> JSON shape spot-checks against the known table.
    assert_eq!(parsed.routes.len(), 1, "single root-layout parent route");
    assert_eq!(parsed.remotes.len(), 10, "ten federation remotes");
    let ep_paths: Vec<&str> = parsed.endpoints.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(ep_paths, vec!["/", "/health", "/posts", "/posts/:id"]);
}

#[test]
fn cli_json_is_deterministic() {
    let app = example_app_dir();
    let a = run_cli(&[app.to_str().unwrap()]);
    let b = run_cli(&[app.to_str().unwrap()]);
    assert_eq!(a, b, "two CLI runs produce byte-identical JSON");
}

#[test]
fn cli_emit_ts_is_a_consumable_route_module() {
    let app = example_app_dir();
    let ts = run_cli(&[app.to_str().unwrap(), "--emit", "ts"]);

    // Module surface a JS app imports.
    assert!(ts.contains("import type { Routes } from '@angular/router'"));
    assert!(ts.contains("export const routes: Routes = ["));
    assert!(ts.contains("export default routes"));
    assert!(ts.contains("export const federationRemotes = "));
    assert!(ts.trim_end().ends_with("as const"));

    // The verified route table: each entry file is a lazy loader with the
    // default ../../ import base, and the nested blog layout/children shape is
    // present. (Matches the example_e2e route table 1:1.)
    let expected_loaders = [
        "../../routes/layout.treaty",
        "../../routes/index.treaty",
        "../../routes/(marketing)/index.treaty",
        "../../routes/(marketing)/about.tjsx",
        "../../routes/blog/layout.treaty",
        "../../routes/blog/index.treaty",
        "../../routes/blog/[...path]/index.treaty",
        "../../routes/blog/[slug]/index.treaty",
        "../../routes/docs/[category]/[page]/index.tjsx",
        "../../routes/not-found.treaty",
    ];
    for entry in expected_loaders {
        let needle = format!("loadComponent: () => import(\"{entry}\")");
        assert!(ts.contains(&needle), "missing loader {needle:?}\n{ts}");
    }

    // Route paths, including the wildcard, dynamic, and group-stripped indexes.
    for path in ["path: \"\"", "path: \"about\"", "path: \"blog\"", "path: \"[slug]\"", "path: \"[...path]\"", "path: \"docs/[category]/[page]\"", "path: \"**\""] {
        assert!(ts.contains(path), "missing route path {path:?}");
    }

    // Federation remotes: the ten known unique names, all exposing ./Route.
    for name in [
        "\"root\"", "\"root-index\"", "\"root-marketing\"", "\"about\"", "\"blog\"",
        "\"root-blog\"", "\"path\"", "\"slug\"", "\"docs-category-page\"", "\"not-found\"",
    ] {
        assert!(ts.contains(&format!("\"name\": {name}")), "missing remote name {name}");
    }
    assert!(ts.contains("\"exposedModule\": \"./Route\""));

    // Deterministic.
    let again = run_cli(&[app.to_str().unwrap(), "--emit", "ts"]);
    assert_eq!(ts, again, "TS emission is deterministic");
}

#[test]
fn cli_colon_style_and_no_federation() {
    let app = example_app_dir();
    let ts = run_cli(&[
        app.to_str().unwrap(),
        "--emit",
        "ts",
        "--style",
        "colon",
        "--no-federation",
    ]);

    // Colon style flips bracket dynamic segments to :param in the route paths
    // (file loaders are unaffected — they are on-disk paths).
    assert!(ts.contains("path: \"docs/:category/:page\""));
    assert!(ts.contains("path: \":slug\""));
    assert!(ts.contains("path: \":...path\""));
    assert!(!ts.contains("path: \"[slug]\""), "no bracket segments survive Colon style");

    // Federation off -> empty remotes array.
    assert!(ts.contains("export const federationRemotes = [] as const"));
}

#[test]
fn cli_custom_dirs_resolve() {
    // Drive the configurable dir names against the real example: its api/ holds
    // endpoints, routes/ holds pages. Pointing --routes-dir at the api/ folder
    // (which has no routable component files but real nested dirs) still scans
    // cleanly and yields a well-formed manifest, proving the flag threads
    // through to the scanner.
    let app = example_app_dir();
    let stdout = run_cli(&[app.to_str().unwrap(), "--routes-dir", "api", "--api-dir", "routes"]);
    let parsed: GeneratedRouting = serde_json::from_str(&stdout).unwrap();
    // api/ has no recognised route component files, but its dynamic [id] dir and
    // index handlers are .ts which IS a routable route extension, so it lowers
    // to routes; the point is the flags are honored and output is valid JSON.
    let direct = generate_routing(
        &FileRoutingConfig::resolve(treaty_file_routing::PartialFileRoutingConfig {
            routes_dir: Some("api".to_string()),
            api_dir: Some("routes".to_string()),
            ..Default::default()
        }),
        &RealFsDirTree::new(&app),
    );
    assert_eq!(parsed, direct, "custom dir flags thread through to the pipeline");
}

#[test]
fn cli_writes_out_file() {
    let app = example_app_dir();
    let out_path = std::env::temp_dir().join(format!(
        "treaty_file_routing_cli_out_{}.ts",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&out_path);

    let confirm = run_cli(&[
        app.to_str().unwrap(),
        "--emit",
        "ts",
        "--out",
        out_path.to_str().unwrap(),
    ]);
    // stdout is a short confirmation, not the module.
    assert!(confirm.contains("wrote"), "expected a write-confirmation line, got {confirm:?}");

    let written = std::fs::read_to_string(&out_path).expect("out file written");
    assert!(written.contains("export const routes: Routes = ["));
    assert!(written.contains("export default routes"));
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn cli_missing_root_errors_with_usage() {
    let out = Command::new(bin()).output().expect("spawn");
    assert!(!out.status.success(), "missing <routes-root> must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("routes-root"), "stderr should mention the missing arg:\n{stderr}");
    assert!(stderr.contains("USAGE"), "stderr should print usage:\n{stderr}");
}

#[test]
fn cli_help_prints_usage_and_exits_zero() {
    let out = Command::new(bin()).arg("--help").output().expect("spawn");
    assert!(out.status.success(), "--help exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("USAGE"));
    assert!(stdout.contains("--emit"));
}
