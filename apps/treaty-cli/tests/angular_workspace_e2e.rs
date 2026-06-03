//! End-to-end tests for the `angular.json`-driven CLI surface.
//!
//! These cover the two halves of the Angular-CLI parity work:
//!
//!   * WORKSPACE RESOLUTION (pure Rust, always runs): parse an `angular.json`,
//!     resolve the `build` architect target (builder + merged options + projected
//!     paths), and confirm `resolve_project` returns the angular.json path (not the
//!     treaty.config fallback) with the entry/outDir taken from the target.
//!
//!   * THE NODE BRIDGE (schematics + migrations): these spawn the REAL
//!     `@angular-devkit` via Node. They are GATED on Node + a resolvable devkit so
//!     a clean checkout without `node_modules` (or without Node) skips rather than
//!     fails — the same honesty the rest of the suite uses. When the toolchain IS
//!     present (the dev/CI box that has run `npm install`), they assert the genuine
//!     devkit creates files.

use std::path::{Path, PathBuf};

use treaty_cli::angular::{self, resolve_architect_target};
use treaty_cli::config::{resolve_project, ConfigOverrides, ProjectConfig};
use treaty_cli::node_cmd::{self, find_node, resolution_roots, SchematicRun};

/// Write a minimal but realistic Angular workspace (an `application` project with a
/// `build` target) into a fresh temp dir, returning the workspace root.
fn workspace_fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("treaty-ngws-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("angular.json"),
        r#"{
          "version": 1,
          "projects": {
            "demo": {
              "projectType": "application",
              "root": "",
              "sourceRoot": "src",
              "prefix": "app",
              "architect": {
                "build": {
                  "builder": "@angular/build:application",
                  "options": {
                    "outputPath": "dist/demo",
                    "index": "src/index.html",
                    "browser": "src/main.ts",
                    "tsConfig": "tsconfig.app.json",
                    "styles": ["src/styles.css"]
                  },
                  "configurations": {
                    "production": { "outputHashing": "all" },
                    "development": { "optimization": false }
                  },
                  "defaultConfiguration": "production"
                }
              }
            }
          }
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src").join("index.html"),
        "<!doctype html><html><body><app-root></app-root></body></html>",
    )
    .unwrap();
    std::fs::write(
        dir.join("src").join("main.ts"),
        "import { Component } from '@angular/core';\n\
         @Component({ selector: 'app-root', template: '<h1>demo</h1>' })\n\
         export class App {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("styles.css"), "body{}").unwrap();
    dir
}

#[test]
fn resolve_project_prefers_angular_json_and_resolves_the_build_target() {
    let dir = workspace_fixture("resolve");

    let resolved = resolve_project(&dir, None, "build", None, &ConfigOverrides::default())
        .expect("resolve_project ok");

    match resolved {
        ProjectConfig::Angular(ang) => {
            assert_eq!(ang.target.project, "demo");
            assert_eq!(ang.target.builder, "@angular/build:application");
            // defaultConfiguration applied.
            assert_eq!(ang.target.configuration.as_deref(), Some("production"));
            // Entry + outDir taken from the architect target, absolute.
            assert!(ang.entry.ends_with("src/main.ts"), "entry: {}", ang.entry.display());
            assert!(
                ang.out_dir.ends_with("dist/demo"),
                "out_dir: {}",
                ang.out_dir.display()
            );
            assert!(ang.entry.is_absolute());
            // The merged option survives.
            assert_eq!(
                ang.target.options.get("outputHashing").and_then(|v| v.as_str()),
                Some("all")
            );
        }
        ProjectConfig::Treaty(_) => panic!("expected the angular.json path, got the treaty fallback"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_project_explicit_configuration_overrides() {
    let dir = workspace_fixture("config");
    let resolved = resolve_project(&dir, Some("demo"), "build", Some("development"), &ConfigOverrides::default())
        .expect("resolve ok");
    let ProjectConfig::Angular(ang) = resolved else {
        panic!("expected angular path");
    };
    assert_eq!(ang.target.configuration.as_deref(), Some("development"));
    assert_eq!(
        ang.target.options.get("optimization").and_then(|v| v.as_bool()),
        Some(false)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_project_falls_back_to_conventions_without_angular_json() {
    // A dir with NO angular.json -> the treaty.config/convention path.
    let dir = std::env::temp_dir().join(format!("treaty-noang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let resolved = resolve_project(&dir, None, "build", None, &ConfigOverrides::default())
        .expect("resolve ok");
    assert!(matches!(resolved, ProjectConfig::Treaty(_)), "expected the convention fallback");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn architect_target_round_trips_through_the_parser() {
    let dir = workspace_fixture("arch");
    let ws = angular::parse_workspace(&dir.join("angular.json")).unwrap();
    let t = resolve_architect_target(&ws, "demo", "build", Some("production")).unwrap();
    assert_eq!(t.styles.len(), 1);
    assert!(t.styles[0].ends_with("src/styles.css"));
    assert!(t.ts_config.unwrap().ends_with("tsconfig.app.json"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether the real `@angular-devkit` toolchain is available from THIS repo (Node
/// on PATH + a resolvable devkit). The schematics/migration tests gate on this so a
/// clean clone without `node_modules` skips rather than fails.
fn devkit_available() -> Option<PathBuf> {
    find_node()?;
    let repo_root = repo_root();
    resolution_roots(&repo_root)
        .into_iter()
        .find(|r| r.join("node_modules/@angular-devkit/schematics").is_dir())
}

/// The treaty repo root (two levels up from this crate's manifest dir).
fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/apps/treaty-cli
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or(manifest)
}

#[test]
fn schematics_generate_creates_real_files_when_devkit_present() {
    let Some(resolve_root) = devkit_available() else {
        eprintln!("skipping: no Node + @angular-devkit available (clean checkout)");
        return;
    };

    // A throwaway workspace UNDER the repo tree so the ancestor-walk finds the
    // repo's devkit/collection in node_modules.
    let dir = repo_root().join(format!(".treaty-sch-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src").join("app")).unwrap();
    std::fs::write(
        dir.join("angular.json"),
        r#"{ "version": 1, "projects": { "demo": {
            "projectType": "application", "root": "", "sourceRoot": "src", "prefix": "app",
            "architect": { "build": { "builder": "@angular/build:application",
              "options": { "browser": "src/main.ts", "tsConfig": "tsconfig.json" } } } } } }"#,
    )
    .unwrap();
    std::fs::write(dir.join("tsconfig.json"), r#"{ "compilerOptions": {} }"#).unwrap();

    // Inject --project (as the CLI's generate handler does) so the component
    // schematic validates exactly as under `ng`.
    let run = SchematicRun {
        project_root: dir.clone(),
        collection: "@schematics/angular".to_string(),
        schematic: "component".to_string(),
        name: Some("itwidget".to_string()),
        dry_run: false,
        force: false,
        passthrough: vec!["--project".into(), "demo".into()],
    };
    let _ = &resolve_root;
    let res = node_cmd::run_schematic(&run);
    assert!(res.is_ok(), "schematic run failed: {:?}", res.err());

    // The genuine devkit created the component files.
    let comp = dir.join("src").join("app").join("itwidget").join("itwidget.ts");
    assert!(comp.is_file(), "component .ts not created at {}", comp.display());
    let body = std::fs::read_to_string(&comp).unwrap();
    assert!(body.contains("@Component"), "not a real Angular component: {body}");
    assert!(body.contains("app-itwidget"), "prefix not applied: {body}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schematics_dry_run_writes_nothing_when_devkit_present() {
    let Some(_) = devkit_available() else {
        eprintln!("skipping: no Node + @angular-devkit available");
        return;
    };
    let dir = repo_root().join(format!(".treaty-sch-dry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src").join("app")).unwrap();
    std::fs::write(
        dir.join("angular.json"),
        r#"{ "version": 1, "projects": { "demo": {
            "projectType": "application", "root": "", "sourceRoot": "src", "prefix": "app",
            "architect": { "build": { "builder": "@angular/build:application",
              "options": { "browser": "src/main.ts", "tsConfig": "tsconfig.json" } } } } } }"#,
    )
    .unwrap();
    std::fs::write(dir.join("tsconfig.json"), r#"{ "compilerOptions": {} }"#).unwrap();

    let run = SchematicRun {
        project_root: dir.clone(),
        collection: "@schematics/angular".to_string(),
        schematic: "service".to_string(),
        name: Some("drysvc".to_string()),
        dry_run: true,
        force: false,
        passthrough: vec!["--project".into(), "demo".into()],
    };
    assert!(node_cmd::run_schematic(&run).is_ok());
    // Dry run: nothing written.
    assert!(
        !dir.join("src").join("app").join("drysvc.ts").exists(),
        "dry run wrote a file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
