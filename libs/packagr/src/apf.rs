//! Angular Package Format (APF) manifest generation.
//!
//! Emits a valid APF `package.json` for a library: a single root manifest that
//! advertises the primary entry and every secondary entry through the modern
//! `exports` map (`types` + `default`/`import` conditions per sub-path) and the
//! legacy top-level `module` / `types` fields, marking the package
//! `sideEffects: false` and `type: module` as APF requires.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::core::EntryPoint;

/// The `exports` conditions for a single entry sub-path.
///
/// `types` is emitted first so resolvers that honour condition order pick up
/// declarations before the runtime module, matching APF's recommendation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportConditions {
    pub types: String,
    /// The ESM entry. Exposed under both `import` (the explicit ESM condition)
    /// and `default` so non-condition-aware resolvers still find it.
    pub import: String,
    pub default: String,
}

impl ExportConditions {
    /// Build the conditions for an entry living under `dir` (relative to the
    /// package root, e.g. `"."` or `"./testing"`).
    fn for_dir(dir: &str) -> Self {
        let base = if dir == "." {
            ".".to_string()
        } else {
            dir.trim_end_matches('/').to_string()
        };
        ExportConditions {
            types: format!("{base}/index.d.ts"),
            import: format!("{base}/index.mjs"),
            default: format!("{base}/index.mjs"),
        }
    }
}

/// The APF `package.json` fields packagr emits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApfManifest {
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub module_type: String,
    /// Legacy primary-entry fields (pre-`exports` resolvers).
    pub module: String,
    pub types: String,
    #[serde(rename = "sideEffects")]
    pub side_effects: bool,
    /// The `package.json#exports` subpath map.
    pub exports: BTreeMap<String, ExportConditions>,
}

/// The relative directory (under the package root) for a published sub-path.
/// `""` → `"."` (primary), `"testing"` → `"./testing"`.
pub fn dir_for_sub_path(sub_path: &str) -> String {
    if sub_path.is_empty() {
        ".".to_string()
    } else {
        format!("./{}", sub_path.trim_start_matches("./"))
    }
}

/// The `exports` key for a published sub-path. `""` → `"."`,
/// `"testing"` → `"./testing"`.
pub fn export_key(sub_path: &str) -> String {
    dir_for_sub_path(sub_path)
}

/// Build the package-level APF manifest covering `name@version` with an exports
/// entry for every published `sub_path` (the empty string being the primary).
pub fn package_manifest(name: &str, version: &str, sub_paths: &[String]) -> ApfManifest {
    let mut exports = BTreeMap::new();
    for sub in sub_paths {
        let key = export_key(sub);
        let dir = dir_for_sub_path(sub);
        exports.insert(key, ExportConditions::for_dir(&dir));
    }
    // Guarantee the primary export is always present.
    exports
        .entry(".".to_string())
        .or_insert_with(|| ExportConditions::for_dir("."));

    ApfManifest {
        name: name.to_string(),
        version: version.to_string(),
        module_type: "module".to_string(),
        module: "./index.mjs".to_string(),
        types: "./index.d.ts".to_string(),
        side_effects: false,
        exports,
    }
}

/// Build the package-level APF manifest JSON (pretty-printed).
pub fn package_manifest_json(name: &str, version: &str, sub_paths: &[String]) -> String {
    let manifest = package_manifest(name, version, sub_paths);
    // The struct is statically valid, so serialization cannot fail; fall back
    // to an empty object only to avoid an unwrap panic in pathological builds.
    serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".to_string())
}

/// Build a single-entry APF manifest JSON for `entry` (the primary-only case).
///
/// Retained for the per-entry [`crate::package`] convenience pipeline; the
/// full library flow uses [`package_manifest_json`].
pub fn manifest(entry: &EntryPoint) -> String {
    package_manifest_json(&entry.name, "0.0.0", &[String::new()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_apf_fields() {
        let entry = EntryPoint {
            name: "@acme/widget".into(),
            file_name: "widget.ts".into(),
            source: String::new(),
        };
        let json = manifest(&entry);
        assert!(json.contains("@acme/widget"));
        assert!(json.contains("\"type\": \"module\""));
        assert!(json.contains("./index.d.ts"));
        assert!(json.contains("\"sideEffects\": false"));
    }

    #[test]
    fn exports_map_covers_primary_and_secondaries() {
        let manifest =
            package_manifest("@acme/widgets", "1.2.3", &["".into(), "testing".into()]);

        // Primary export under ".".
        let primary = &manifest.exports["."];
        assert_eq!(primary.types, "./index.d.ts");
        assert_eq!(primary.import, "./index.mjs");
        assert_eq!(primary.default, "./index.mjs");

        // Secondary export under "./testing".
        let testing = &manifest.exports["./testing"];
        assert_eq!(testing.types, "./testing/index.d.ts");
        assert_eq!(testing.import, "./testing/index.mjs");

        // APF legacy + flags.
        assert_eq!(manifest.module_type, "module");
        assert!(!manifest.side_effects);
        assert_eq!(manifest.module, "./index.mjs");
        assert_eq!(manifest.types, "./index.d.ts");
    }

    #[test]
    fn json_shape_is_apf_valid() {
        let json = package_manifest_json("@acme/widgets", "1.0.0", &["".into(), "testing".into()]);
        assert!(json.contains("\"@acme/widgets\""));
        assert!(json.contains("\"./testing\""));
        assert!(json.contains("\"import\": \"./testing/index.mjs\""));
        assert!(json.contains("\"types\": \"./testing/index.d.ts\""));
    }
}
