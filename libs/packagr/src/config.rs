//! Library package configuration and entry-point discovery.
//!
//! packagr is driven by a `treaty-package.json` (or, for drop-in compatibility,
//! an `ng-package.json`) descriptor that mirrors the relevant subset of
//! ng-packagr's schema:
//!
//! ```json
//! {
//!   "dest": "../../dist/widgets",
//!   "lib": { "entryFile": "src/public-api.ts" }
//! }
//! ```
//!
//! Secondary entry points are, exactly as in ng-packagr, discovered from the
//! filesystem: any subdirectory of the package root that contains its own
//! `treaty-package.json` / `ng-package.json` (or, as a convenience, a bare
//! `public-api`/`index` source file) is a secondary entry point. They may also
//! be listed explicitly via `secondaryEntryPoints` for sources that do not
//! follow the on-disk convention.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::PackagrError;

/// The `lib` block of a package descriptor (the primary entry's source).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LibConfig {
    /// The primary entry source, relative to the descriptor, e.g.
    /// `src/public-api.ts`. Defaults to `src/public-api.ts` when omitted.
    #[serde(rename = "entryFile", default)]
    pub entry_file: Option<String>,
}

/// An explicitly-listed secondary entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecondaryEntryConfig {
    /// The sub-path appended to the primary package name, e.g. `testing` for
    /// `@acme/widgets/testing`. Inferred from the directory when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// This entry's source file, relative to the descriptor.
    #[serde(rename = "entryFile")]
    pub entry_file: String,
}

/// A parsed `treaty-package.json` / `ng-package.json` descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PackageConfig {
    /// The published package name (e.g. `@acme/widgets`). When omitted it is
    /// read from a sibling `package.json`'s `name`, exactly as ng-packagr does.
    #[serde(default)]
    pub name: Option<String>,
    /// The published package version. Falls back to the sibling
    /// `package.json`'s `version`, then `0.0.0`.
    #[serde(default)]
    pub version: Option<String>,
    /// The output directory, relative to the descriptor. Defaults to `dist`.
    #[serde(default)]
    pub dest: Option<String>,
    /// The primary entry configuration.
    #[serde(default)]
    pub lib: LibConfig,
    /// Explicitly-declared secondary entry points (in addition to any
    /// discovered on disk).
    #[serde(rename = "secondaryEntryPoints", default)]
    pub secondary_entry_points: Vec<SecondaryEntryConfig>,
    /// Asset globs/paths to copy verbatim into `dest` (e.g. `README.md`).
    #[serde(default)]
    pub assets: Vec<String>,
}

impl PackageConfig {
    /// Parse a descriptor from its JSON text.
    pub fn from_json(text: &str) -> Result<Self, PackagrError> {
        serde_json::from_str(text).map_err(|e| PackagrError::Config(e.to_string()))
    }

    /// The resolved primary entry source path, defaulting to
    /// `src/public-api.ts` when unspecified.
    pub fn entry_file(&self) -> &str {
        self.lib
            .entry_file
            .as_deref()
            .unwrap_or("src/public-api.ts")
    }

    /// The resolved output directory, defaulting to `dist`.
    pub fn dest(&self) -> &str {
        self.dest.as_deref().unwrap_or("dist")
    }
}

/// The descriptor file names packagr recognises, in precedence order.
pub const DESCRIPTOR_NAMES: [&str; 2] = ["treaty-package.json", "ng-package.json"];

/// The source file stems treated as an implicit entry point.
const IMPLICIT_ENTRY_STEMS: [&str; 2] = ["public-api", "index"];

/// The authoring/source extensions an implicit entry may use, in precedence
/// order (authoring extensions first so a `.treaty` SFC wins over a `.ts`).
const ENTRY_EXTENSIONS: [&str; 4] = ["treaty", "tsx", "tjsx", "ts"];

/// Locate the package descriptor inside `dir`, returning its path.
pub fn find_descriptor(dir: &Path) -> Option<PathBuf> {
    DESCRIPTOR_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

/// The package name for an entry, derived from `<stem>.<ext>` of the source's
/// containing layout. Used only as a fallback when no `name` is otherwise
/// known.
fn dir_name_of(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

/// Find the implicit entry source inside `dir` (a `public-api`/`index` file in
/// the directory itself or its `src/` subfolder), if one exists.
fn implicit_entry_in(dir: &Path) -> Option<PathBuf> {
    for base in [dir.to_path_buf(), dir.join("src")] {
        for stem in IMPLICIT_ENTRY_STEMS {
            for ext in ENTRY_EXTENSIONS {
                let candidate = base.join(format!("{stem}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// A resolved entry point: where its source lives on disk and the published
/// sub-path (empty for the primary entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry {
    /// The published sub-path under the primary package name (`""` = primary,
    /// `"testing"` = `@scope/pkg/testing`).
    pub sub_path: String,
    /// The absolute path to this entry's source file.
    pub source_path: PathBuf,
}

impl ResolvedEntry {
    /// Whether this is the primary entry point.
    pub fn is_primary(&self) -> bool {
        self.sub_path.is_empty()
    }
}

/// Discover the primary and secondary entry points for the package rooted at
/// `package_dir`, given its parsed `config`.
///
/// Discovery rules (matching ng-packagr's model):
///   * the primary entry is `config.lib.entryFile`, resolved against
///     `package_dir`;
///   * every subdirectory of `package_dir` that carries its own descriptor —
///     or, as a convenience, an implicit `public-api`/`index` source — is a
///     secondary entry point, its sub-path being the directory name;
///   * any `config.secondaryEntryPoints` are added on top (explicit wins on
///     sub-path collisions).
pub fn discover_entries(
    package_dir: &Path,
    config: &PackageConfig,
) -> Result<Vec<ResolvedEntry>, PackagrError> {
    let mut entries = Vec::new();

    // Primary.
    let primary_source = package_dir.join(config.entry_file());
    // The top-level directory the primary entry lives under (e.g. `src` for
    // `src/public-api.ts`) is part of the primary entry, never a secondary.
    let primary_top_dir = Path::new(config.entry_file())
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(str::to_string);
    entries.push(ResolvedEntry {
        sub_path: String::new(),
        source_path: primary_source,
    });

    // Secondary, discovered on disk.
    if let Ok(read) = std::fs::read_dir(package_dir) {
        let mut discovered: Vec<ResolvedEntry> = Vec::new();
        for dirent in read.flatten() {
            let path = dirent.path();
            if !path.is_dir() {
                continue;
            }
            // `node_modules`, the output `dest`, and dotfolders are never entry
            // points.
            let name = match dir_name_of(&path) {
                Some(n) if !n.starts_with('.') && n != "node_modules" => n,
                _ => continue,
            };
            if Path::new(config.dest()).file_name().and_then(|d| d.to_str()) == Some(name.as_str())
            {
                continue;
            }
            // The directory holding the primary entry source is not a secondary.
            if primary_top_dir.as_deref() == Some(name.as_str()) {
                continue;
            }

            let source = if let Some(desc) = find_descriptor(&path) {
                let sub_cfg = PackageConfig::from_json(
                    &std::fs::read_to_string(&desc).map_err(PackagrError::from)?,
                )?;
                path.join(sub_cfg.entry_file())
            } else if let Some(implicit) = implicit_entry_in(&path) {
                implicit
            } else {
                continue;
            };

            discovered.push(ResolvedEntry {
                sub_path: name,
                source_path: source,
            });
        }
        // Deterministic ordering keeps the exports map / tests stable.
        discovered.sort_by(|a, b| a.sub_path.cmp(&b.sub_path));
        entries.extend(discovered);
    }

    // Explicitly-listed secondary entries (override discovered on collision).
    for sec in &config.secondary_entry_points {
        let sub_path = sec
            .path
            .clone()
            .or_else(|| {
                Path::new(&sec.entry_file)
                    .parent()
                    .and_then(dir_name_of)
            })
            .unwrap_or_default();
        let source_path = package_dir.join(&sec.entry_file);
        if let Some(existing) = entries.iter_mut().find(|e| e.sub_path == sub_path) {
            existing.source_path = source_path;
        } else {
            entries.push(ResolvedEntry {
                sub_path,
                source_path,
            });
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        let cfg = PackageConfig::from_json("{}").unwrap();
        assert_eq!(cfg.entry_file(), "src/public-api.ts");
        assert_eq!(cfg.dest(), "dist");
    }

    #[test]
    fn parses_ng_package_shape() {
        let cfg = PackageConfig::from_json(
            r#"{ "dest": "../dist/widgets", "lib": { "entryFile": "src/index.treaty" } }"#,
        )
        .unwrap();
        assert_eq!(cfg.entry_file(), "src/index.treaty");
        assert_eq!(cfg.dest(), "../dist/widgets");
    }
}
