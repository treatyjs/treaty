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
//! Each entry's `index.mjs` is a *flattened* FESM module: [`build_entry`] runs
//! [`crate::fesm::flatten_entry_esm`] after compilation, inlining the entry's own
//! private internal modules into one ES module while leaving bare specifiers and
//! sibling published entries as imports. A single-file entry flattens to itself
//! (identity). This slots between compilation and manifest assembly without
//! changing [`package_library`]'s signature.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::apf;
use crate::compile;
use crate::config::{self, CompilationMode, PackageConfig, ResolvedEntry};
use crate::core::{DistEntry, DistManifest, PackagrError};
use crate::dts;
use crate::dts_flatten;
use crate::fesm;
use crate::stylesheet;

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

/// Compile one resolved entry to its ESM + `.d.ts` artifacts, then flatten its
/// private internal modules into a single APF FESM module.
///
/// `entry_source_set` is the canonical set of *all* entry source paths; the
/// flatten step uses it to distinguish a private helper (inlined) from a sibling
/// published entry (left as a cross-entry reference). The `.d.ts` is derived from
/// the un-flattened compile so isolated-declarations/component reconstruction
/// continue to see the entry's own source surface.
fn build_entry(
    entry: &ResolvedEntry,
    entry_source_set: &HashSet<PathBuf>,
    mode: CompilationMode,
) -> Result<DistEntry, PackagrError> {
    let source = std::fs::read_to_string(&entry.source_path).map_err(PackagrError::from)?;
    let file_name = entry
        .source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("index.ts");

    // Compile with the entry's on-disk path known, so a `@Component` declaring
    // external `styleUrls`/`styleUrl` has those files resolved + preprocessed
    // (SCSS/Sass) and folded into its scoped `styles: [...]` — exactly ng-packagr's
    // pre-`ngc` step. For authoring sources and inline-only-style entries this is
    // byte-identical to `compile_entry`.
    let esm = compile::compile_entry_at(&source, &entry.source_path);
    if !esm.errors.is_empty() {
        return Err(PackagrError::Compile(esm.errors));
    }
    // The `.d.ts` is derived from the AOT compile and is IDENTICAL regardless of
    // compilation mode (the declared type surface does not change between full and
    // partial emit), so it is always derived from the AOT `esm.code`.
    let entry_dts = dts::emit_dts_for_entry(&source, &esm.code, file_name)?;
    // Flatten a re-export BARREL declaration into a self-contained `index.d.ts`:
    // resolve each `export * from './lib/x'` / `export { X } from '…'` on disk,
    // inline the re-exported type declarations, and emit one aggregated `export
    // { … }` (the `.d.ts` analogue of the FESM flatten — see [`crate::dts_flatten`]).
    // A non-barrel entry is returned unchanged.
    let declarations = dts_flatten::flatten_entry_dts(
        &source,
        &entry_dts,
        &entry.source_path,
        entry_source_set,
    );

    // Flatten the entry's own internal modules into one FESM module. For a
    // single-file entry this is a byte-for-byte identity.
    let flat_esm = fesm::flatten_entry_esm(&esm.code, &entry.source_path, entry_source_set);

    // esbuild-equivalent CSS value minification over every emitted `styles: [...]`
    // string (the pre-`ngc` esbuild `minify: true` pass ng-packagr runs): `color:
    // blue` → `#00f`, `0px` → `0`, whitespace collapse — while preserving the
    // `_ngcontent-%COMP%` scoping placeholders. Run over the FLATTENED module so it
    // covers both the entry's own styles AND those of any inlined private component
    // module; a module with no styles is returned byte-for-byte unchanged.
    let flat_esm = stylesheet::optimize_compiled_styles(&flat_esm);

    // PARTIAL compilation mode: rewrite the flattened AOT module's DI/pipe-family
    // `ɵɵdefine*` definitions to their `ɵɵngDeclare*` partial form (the Angular-CLI
    // library publish format). Mode-gated — `Full` (the default) returns the AOT
    // module unchanged, byte-for-byte. The partial form round-trips back to the AOT
    // form through the Angular linker (verified in `treaty_ivy::partial_emit`).
    let esm_out = match mode {
        CompilationMode::Full => flat_esm,
        CompilationMode::Partial => treaty_ivy::emit_partial(&flat_esm).code,
    };

    Ok(DistEntry {
        sub_path: entry.sub_path.clone(),
        dir: apf::dir_for_sub_path(&entry.sub_path),
        esm: esm_out,
        declarations,
    })
}

/// One resolved asset to copy into the dist root: its destination path relative
/// to `dest` (preserving the glob-matched directory structure) and its raw bytes
/// (binary-safe, so `.svg`/`.png`/font assets copy verbatim).
#[derive(Debug, Clone)]
pub struct CollectedAsset {
    /// The path under the dist root to write to (e.g. `assets/icons/x.svg`).
    pub rel_path: String,
    /// The file's raw bytes.
    pub bytes: Vec<u8>,
}

/// Resolve the assets declared in `config` to their copyable bytes + dist paths.
///
/// Each `assets` entry is resolved relative to `package_dir`. A plain path
/// (`README.md`) copies that one file to the dist root by its file name. A GLOB
/// pattern (`assets/**/*.{svg,png}`) is expanded against the package root and each
/// match copies into the dist preserving its path RELATIVE to the glob's
/// non-wildcard base directory — so `assets/icons/x.svg` lands at
/// `<dest>/assets/icons/x.svg` (ng-packagr's asset-copy behaviour). The configured
/// `readmeFile`/`licenseFile` (or, when unset, an implicit root `README.md` /
/// `LICENSE*`) are appended.
///
/// Missing assets / non-matching globs are skipped rather than failing the build
/// (matching ng-packagr's lenient copy step). Assets are read as raw bytes so
/// binary files copy verbatim.
fn collect_assets(package_dir: &Path, config: &PackageConfig) -> Vec<CollectedAsset> {
    let mut out: Vec<CollectedAsset> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push = |rel_path: String, bytes: Vec<u8>, out: &mut Vec<CollectedAsset>, seen: &mut HashSet<String>| {
        if seen.insert(rel_path.clone()) {
            out.push(CollectedAsset { rel_path, bytes });
        }
    };

    for asset in &config.assets {
        if is_glob(asset) {
            // Expand the glob against the package root; preserve each match's path
            // relative to the glob's literal base directory.
            let base = glob_base_dir(asset);
            let pattern = package_dir.join(asset);
            let Some(pattern_str) = pattern.to_str() else { continue };
            if let Ok(paths) = glob::glob(pattern_str) {
                for entry in paths.flatten() {
                    if !entry.is_file() {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&entry) else { continue };
                    // The path relative to the package root, keeping the glob's
                    // base directory prefix (`assets/...`).
                    let rel = entry
                        .strip_prefix(package_dir)
                        .ok()
                        .and_then(|p| p.to_str())
                        .map(|s| s.replace('\\', "/"))
                        .unwrap_or_else(|| {
                            // Fallback: base dir + file name.
                            let fname = entry.file_name().and_then(|n| n.to_str()).unwrap_or("asset");
                            if base.is_empty() {
                                fname.to_string()
                            } else {
                                format!("{base}/{fname}")
                            }
                        });
                    push(rel, bytes, &mut out, &mut seen);
                }
            }
        } else {
            let src = package_dir.join(asset);
            if let Ok(bytes) = std::fs::read(&src) {
                // A plain asset copies to the dist root by its file name.
                let file_name = Path::new(asset)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(asset)
                    .to_string();
                push(file_name, bytes, &mut out, &mut seen);
            }
        }
    }

    // README + LICENSE auto-copy. An explicit `readmeFile`/`licenseFile` wins;
    // otherwise fall back to the conventional root files if present.
    for (explicit, fallbacks) in [
        (&config.readme_file, &["README.md", "README"][..]),
        (
            &config.license_file,
            &["LICENSE", "LICENSE.md", "LICENSE.txt"][..],
        ),
    ] {
        if let Some(explicit) = explicit {
            if let Ok(bytes) = std::fs::read(package_dir.join(explicit)) {
                let name = Path::new(explicit)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(explicit)
                    .to_string();
                push(name, bytes, &mut out, &mut seen);
            }
        } else {
            for candidate in fallbacks {
                if let Ok(bytes) = std::fs::read(package_dir.join(candidate)) {
                    push(candidate.to_string(), bytes, &mut out, &mut seen);
                    break;
                }
            }
        }
    }

    out
}

/// Whether an asset entry is a glob pattern (carries a `*`, `?`, `[`, or `{`).
fn is_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[') || s.contains('{')
}

/// The literal (non-wildcard) leading directory of a glob pattern, with forward
/// slashes (`assets/**/*.svg` → `assets`). Used to preserve match structure.
fn glob_base_dir(pattern: &str) -> String {
    let normalized = pattern.replace('\\', "/");
    let mut base = Vec::new();
    for seg in normalized.split('/') {
        if is_glob(seg) {
            break;
        }
        base.push(seg);
    }
    base.join("/")
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

    // The canonical set of every entry's source path. The FESM flattener inlines
    // a relative import only when it resolves to a PRIVATE module — one that is
    // not in this set — leaving sibling published entries as cross-entry refs.
    let entry_source_paths: Vec<PathBuf> =
        resolved.iter().map(|e| e.source_path.clone()).collect();
    let entry_source_set = fesm::canonical_entry_set(&entry_source_paths);

    let mode = config.compilation_mode();
    let mut entries = Vec::with_capacity(resolved.len());
    for entry in &resolved {
        entries.push(build_entry(entry, &entry_source_set, mode)?);
    }

    let sub_paths: Vec<String> = entries.iter().map(|e| e.sub_path.clone()).collect();
    let manifest = apf::package_manifest_json(&name, &version, &sub_paths);

    let assets = collect_assets(package_dir, config);
    let asset_names = assets.iter().map(|a| a.rel_path.clone()).collect();

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
    for asset in collect_assets(package_dir, config) {
        // The dest path may carry a glob-preserved sub-directory; create it.
        let target = dest.join(&asset.rel_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(PackagrError::from)?;
        }
        std::fs::write(&target, &asset.bytes).map_err(PackagrError::from)?;
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

    #[test]
    fn glob_assets_and_readme_license_auto_copy() {
        let root = scratch("assets_glob");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("treaty-package.json"),
            r#"{ "name": "@acme/assets", "dest": "dist", "assets": ["assets/**/*.svg"] }"#,
        )
        .unwrap();
        std::fs::write(root.join("src/public-api.ts"), "export const A: number = 1;").unwrap();

        // A nested glob-matched binary-ish asset.
        std::fs::create_dir_all(root.join("assets/icons")).unwrap();
        std::fs::write(root.join("assets/icons/star.svg"), "<svg></svg>").unwrap();
        std::fs::write(root.join("assets/logo.svg"), "<svg/>").unwrap();
        // A non-matching file is NOT copied.
        std::fs::write(root.join("assets/notes.txt"), "ignore me").unwrap();
        // Implicit README + LICENSE auto-copy (no explicit `assets`/`readmeFile` entry).
        std::fs::write(root.join("README.md"), "# readme").unwrap();
        std::fs::write(root.join("LICENSE"), "MIT").unwrap();

        let cfg = PackageConfig::from_json(
            &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
        )
        .unwrap();
        let dist = build_to_disk(&root, &cfg).expect("build should succeed");

        let dest = root.join("dist");
        // The glob matches preserve their directory structure under the dist root.
        assert!(dest.join("assets/icons/star.svg").is_file(), "nested glob asset not copied");
        assert!(dest.join("assets/logo.svg").is_file(), "top glob asset not copied");
        assert!(!dest.join("assets/notes.txt").exists(), "non-matching file copied");
        // README + LICENSE auto-copied to the dist root.
        assert!(dest.join("README.md").is_file(), "README not auto-copied");
        assert!(dest.join("LICENSE").is_file(), "LICENSE not auto-copied");

        // The asset name list records every copied path (glob-relative + readme/license).
        assert!(dist.assets.iter().any(|a| a == "assets/icons/star.svg"));
        assert!(dist.assets.iter().any(|a| a == "README.md"));
        assert!(dist.assets.iter().any(|a| a == "LICENSE"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn partial_compilation_mode_emits_ng_declare_and_round_trips() {
        let root = scratch("partial_mode");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("treaty-package.json"),
            r#"{ "name": "@acme/pipes", "version": "1.0.0", "dest": "dist",
                 "compilationMode": "partial",
                 "lib": { "entryFile": "src/public-api.ts" } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/public-api.ts"),
            "import { Pipe } from '@angular/core';\n\
             @Pipe({ name: 'shout', standalone: true })\n\
             export class ShoutPipe { transform(v: string): string { return v.toUpperCase(); } }\n",
        )
        .unwrap();

        let cfg = PackageConfig::from_json(
            &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cfg.compilation_mode(), CompilationMode::Partial);

        let dist = package_library(&root, &cfg).expect("partial build should succeed");
        let primary = &dist.entries[0];

        // The published ESM is in PARTIAL format: ɵɵngDeclare*, no AOT ɵɵdefine*.
        assert!(
            primary.esm.contains("\u{0275}\u{0275}ngDeclarePipe"),
            "partial mode must emit ɵɵngDeclarePipe; got:\n{}",
            primary.esm
        );
        assert!(
            primary.esm.contains("\u{0275}\u{0275}ngDeclareFactory"),
            "partial mode must emit ɵɵngDeclareFactory; got:\n{}",
            primary.esm
        );
        assert!(
            !primary.esm.contains("\u{0275}\u{0275}definePipe"),
            "partial mode must NOT emit AOT ɵɵdefinePipe; got:\n{}",
            primary.esm
        );

        // ROUND-TRIP: the published partial module must link back through the
        // Angular linker to a valid AOT module carrying the ɵɵdefinePipe. Treaty's
        // emitted module is TypeScript-flavoured ESM (it does not strip type
        // annotations — that is a downstream bundler concern), so link it under a
        // `.ts` name to parse those annotations, exactly as the bundler-integrated
        // linker sees post-transpile source.
        let relinked = treaty_ivy::link_partial(&primary.esm, "index.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(
            relinked.code.contains("\u{0275}\u{0275}definePipe"),
            "linker must restore ɵɵdefinePipe from the partial form; got:\n{}",
            relinked.code
        );
        assert!(
            !relinked.code.contains("\u{0275}\u{0275}ngDeclare"),
            "no ɵɵngDeclare may survive linking; got:\n{}",
            relinked.code
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn full_mode_is_default_and_unchanged_by_partial_plumbing() {
        // A library with NO compilationMode (or "full") emits the AOT ɵɵdefine* form,
        // byte-identical to before the partial-mode plumbing.
        let root = scratch("full_default");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("treaty-package.json"),
            r#"{ "name": "@acme/pipes", "version": "1.0.0", "dest": "dist",
                 "lib": { "entryFile": "src/public-api.ts" } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/public-api.ts"),
            "import { Pipe } from '@angular/core';\n\
             @Pipe({ name: 'shout', standalone: true })\n\
             export class ShoutPipe { transform(v: string): string { return v; } }\n",
        )
        .unwrap();

        let cfg = PackageConfig::from_json(
            &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cfg.compilation_mode(), CompilationMode::Full);
        let dist = package_library(&root, &cfg).unwrap();
        let primary = &dist.entries[0];
        assert!(primary.esm.contains("\u{0275}\u{0275}definePipe"), "full mode must emit AOT define");
        assert!(!primary.esm.contains("\u{0275}\u{0275}ngDeclare"), "full mode must NOT emit ngDeclare");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parses_full_ng_package_json_fields() {
        // A full ng-package.json with fields packagr accepts for compatibility must parse.
        let cfg = PackageConfig::from_json(
            r#"{
                "$schema": "./node_modules/ng-packagr/ng-package.schema.json",
                "dest": "../dist/lib",
                "lib": {
                    "entryFile": "src/public-api.ts",
                    "flatModuleFile": "my-lib",
                    "umdModuleIds": { "lodash": "_" },
                    "cssUrl": "inline",
                    "styleIncludePaths": ["src/styles"]
                },
                "assets": ["README.md", "assets/**/*.svg"],
                "inlineStyleLanguage": "scss",
                "allowedNonPeerDependencies": ["tslib"],
                "keepLifecycleScripts": true
            }"#,
        )
        .expect("full ng-package.json should parse");
        assert_eq!(cfg.entry_file(), "src/public-api.ts");
        assert_eq!(cfg.dest(), "../dist/lib");
        assert_eq!(cfg.lib.flat_module_file.as_deref(), Some("my-lib"));
        assert_eq!(cfg.inline_style_language.as_deref(), Some("scss"));
        assert_eq!(cfg.allowed_non_peer_dependencies, vec!["tslib".to_string()]);
        // An unrecognised top-level key (`keepLifecycleScripts`) is ignored, not an error.
    }
}
