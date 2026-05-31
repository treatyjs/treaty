//! The top-level library packaging pipeline.
//!
//! [`package_library`] is the packagr equivalent of an ng-packagr build: given
//! a package root and its parsed descriptor it discovers every entry point,
//! compiles each to Ivy ESM through the committed front-ends, derives a
//! `.d.ts` per entry via `oxc_isolated_declarations`, and assembles a single
//! Angular Package Format `package.json` whose `exports` map covers the
//! primary entry plus each secondary. The returned [`DistManifest`] holds every
//! artifact in memory; [`DistManifest::write_to`] commits it to disk.
//!
//! Entries emit per-entry ESM today (one `index.mjs` per entry). FESM
//! flattening — collapsing each entry's internal modules into a single
//! flattened ES module with `rolldown` — is the documented next step and would
//! slot in between compilation and manifest assembly without changing this
//! signature.

use std::path::Path;

use crate::apf;
use crate::compile;
use crate::config::{self, PackageConfig, ResolvedEntry};
use crate::core::{DistEntry, DistManifest, PackagrError};
use crate::dts;

/// Read the published name/version, preferring the descriptor's own fields and
/// otherwise falling back to a sibling `package.json`.
fn resolve_name_version(package_dir: &Path, config: &PackageConfig) -> (String, String) {
    let mut name = config.name.clone();
    let mut version = config.version.clone();

    if (name.is_none() || version.is_none())
        && let Ok(text) = std::fs::read_to_string(package_dir.join("package.json"))
        && let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text)
    {
        if name.is_none() {
            name = pkg.get("name").and_then(|v| v.as_str()).map(str::to_string);
        }
        if version.is_none() {
            version = pkg
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
    }

    (
        name.unwrap_or_else(|| "library".to_string()),
        version.unwrap_or_else(|| "0.0.0".to_string()),
    )
}

/// Compile one resolved entry to its ESM + `.d.ts` artifacts.
fn build_entry(entry: &ResolvedEntry) -> Result<DistEntry, PackagrError> {
    let source = std::fs::read_to_string(&entry.source_path).map_err(PackagrError::from)?;
    let file_name = entry
        .source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("index.ts");

    let esm = compile::compile_entry(&source, file_name);
    if !esm.errors.is_empty() {
        return Err(PackagrError::Compile(esm.errors));
    }
    let declarations = dts::emit_dts_for_entry(&source, &esm.code, file_name)?;

    Ok(DistEntry {
        sub_path: entry.sub_path.clone(),
        dir: apf::dir_for_sub_path(&entry.sub_path),
        esm: esm.code,
        declarations,
    })
}

/// Resolve the assets named in `config` to their copyable file names.
///
/// Assets are resolved relative to `package_dir`; missing assets are skipped
/// rather than failing the build (matching ng-packagr's lenient copy step).
fn collect_assets(package_dir: &Path, config: &PackageConfig) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for asset in &config.assets {
        let src = package_dir.join(asset);
        if let Ok(contents) = std::fs::read_to_string(&src) {
            let file_name = Path::new(asset)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(asset)
                .to_string();
            out.push((file_name, contents));
        }
    }
    out
}

/// Package the library rooted at `package_dir` described by `config`.
///
/// Discovers the primary + secondary entry points, compiles and declares each,
/// and produces an in-memory [`DistManifest`] (APF `package.json` + per-entry
/// ESM/`.d.ts` + copied assets). Persisting is the caller's job via
/// [`DistManifest::write_to`].
pub fn package_library(
    package_dir: &Path,
    config: &PackageConfig,
) -> Result<DistManifest, PackagrError> {
    let (name, version) = resolve_name_version(package_dir, config);
    let resolved = config::discover_entries(package_dir, config)?;

    let mut entries = Vec::with_capacity(resolved.len());
    for entry in &resolved {
        entries.push(build_entry(entry)?);
    }

    let sub_paths: Vec<String> = entries.iter().map(|e| e.sub_path.clone()).collect();
    let manifest = apf::package_manifest_json(&name, &version, &sub_paths);

    let assets = collect_assets(package_dir, config);
    let asset_names = assets.iter().map(|(n, _)| n.clone()).collect();

    Ok(DistManifest {
        name,
        version,
        manifest,
        entries,
        assets: asset_names,
    })
}

/// Locate the descriptor in `package_dir`, parse it, and package the library.
///
/// A convenience wrapper over [`package_library`] for the common case where the
/// descriptor lives at the package root.
pub fn package_library_at(package_dir: &Path) -> Result<DistManifest, PackagrError> {
    let descriptor = config::find_descriptor(package_dir).ok_or_else(|| {
        PackagrError::Config(format!(
            "no {} found in {}",
            config::DESCRIPTOR_NAMES.join(" / "),
            package_dir.display()
        ))
    })?;
    let text = std::fs::read_to_string(&descriptor).map_err(PackagrError::from)?;
    let config = PackageConfig::from_json(&text)?;
    package_library(package_dir, &config)
}

/// Package a library and write the whole dist tree under `package_dir/<dest>`,
/// copying assets into the dist root. Returns the produced [`DistManifest`].
pub fn build_to_disk(
    package_dir: &Path,
    config: &PackageConfig,
) -> Result<DistManifest, PackagrError> {
    let dist = package_library(package_dir, config)?;
    let dest = package_dir.join(config.dest());
    dist.write_to(&dest)?;
    for (file_name, contents) in collect_assets(package_dir, config) {
        std::fs::write(dest.join(file_name), contents).map_err(PackagrError::from)?;
    }
    Ok(dist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A unique scratch dir under the OS temp folder.
    fn scratch(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("treaty_packagr_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovers_primary_and_secondary_entries() {
        let root = scratch("discover");
        std::fs::write(
            root.join("treaty-package.json"),
            r#"{ "name": "@acme/widgets", "lib": { "entryFile": "src/public-api.ts" } }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/public-api.ts"), "export const A = 1;").unwrap();

        // Secondary via implicit public-api.
        std::fs::create_dir_all(root.join("testing/src")).unwrap();
        std::fs::write(root.join("testing/src/public-api.ts"), "export const T = 1;").unwrap();

        // Secondary via its own descriptor.
        std::fs::create_dir_all(root.join("forms")).unwrap();
        std::fs::write(
            root.join("forms/ng-package.json"),
            r#"{ "lib": { "entryFile": "api.ts" } }"#,
        )
        .unwrap();
        std::fs::write(root.join("forms/api.ts"), "export const F = 1;").unwrap();

        let cfg = PackageConfig::from_json(
            &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
        )
        .unwrap();
        let entries = config::discover_entries(&root, &cfg).unwrap();

        let sub_paths: Vec<&str> = entries.iter().map(|e| e.sub_path.as_str()).collect();
        assert_eq!(sub_paths, vec!["", "forms", "testing"]);
        assert!(entries[0].is_primary());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn packages_treaty_entry_to_ivy_and_dts() {
        let root = scratch("treaty_entry");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("treaty-package.json"),
            r#"{ "name": "@acme/greeting", "version": "2.0.0",
                 "lib": { "entryFile": "src/public-api.treaty" },
                 "assets": ["README.md"] }"#,
        )
        .unwrap();
        // A real `.treaty` SFC: template + interpolation lowered to Ivy.
        std::fs::write(
            root.join("src/public-api.treaty"),
            "const name = 'World';\n<div>{{ name }}</div>",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "# Greeting").unwrap();

        let cfg = PackageConfig::from_json(
            &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
        )
        .unwrap();
        let dist = package_library(&root, &cfg).expect("packaging should succeed");

        assert_eq!(dist.name, "@acme/greeting");
        assert_eq!(dist.version, "2.0.0");
        assert_eq!(dist.entries.len(), 1);

        let primary = &dist.entries[0];
        assert!(primary.is_primary());
        // Lowered to a real Ivy component definition.
        assert!(
            primary.esm.contains("\u{0275}\u{0275}defineComponent"),
            "expected Ivy defineComponent in ESM; got: {}",
            primary.esm
        );
        assert!(primary.esm.contains("import * as i0 from \"@angular/core\";"));
        // A `.d.ts` was emitted (the SFC declares a component class).
        assert!(!primary.declarations.is_empty());

        // The manifest exposes the primary entry under "." with import/types.
        assert!(dist.manifest.contains("\"@acme/greeting\""));
        assert!(dist.manifest.contains("\"2.0.0\""));
        assert!(dist.manifest.contains("\"import\": \"./index.mjs\""));
        assert!(dist.manifest.contains("\"types\": \"./index.d.ts\""));

        // The README asset was discovered.
        assert_eq!(dist.assets, vec!["README.md".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn writes_dist_tree_with_exports_map() {
        let root = scratch("build_disk");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("treaty-package.json"),
            r#"{ "name": "@acme/widgets", "dest": "dist" }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/public-api.ts"),
            "export const VERSION: string = '1';",
        )
        .unwrap();

        std::fs::create_dir_all(root.join("testing")).unwrap();
        std::fs::write(
            root.join("testing/public-api.ts"),
            "export const helper: number = 1;",
        )
        .unwrap();

        let cfg = PackageConfig::from_json(
            &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
        )
        .unwrap();
        let dist = build_to_disk(&root, &cfg).expect("build should succeed");

        let dest = root.join("dist");
        assert!(dest.join("package.json").is_file());
        assert!(dest.join("index.mjs").is_file());
        assert!(dest.join("index.d.ts").is_file());
        assert!(dest.join("testing/index.mjs").is_file());
        assert!(dest.join("testing/index.d.ts").is_file());

        // Two entries: primary + testing.
        assert_eq!(dist.entries.len(), 2);
        assert!(dist.manifest.contains("\"./testing\""));

        std::fs::remove_dir_all(&root).ok();
    }
}
