//! Shared value types for the packagr pipeline.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A library entry point to be packaged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPoint {
    /// The published package (sub)entry name, e.g. `@acme/widget`.
    pub name: String,
    /// The entry's file name (drives front-end selection + `.d.ts` naming).
    pub file_name: String,
    /// The entry's source text.
    pub source: String,
}

/// The artifacts produced for one entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOutput {
    /// The published entry name (mirrors [`EntryPoint::name`]).
    pub name: String,
    /// The compiled ESM module source.
    pub esm: String,
    /// The generated TypeScript declaration (`.d.ts`) source.
    pub declarations: String,
    /// The APF `package.json` manifest, pretty-printed JSON.
    pub manifest: String,
}

impl PackageOutput {
    /// Write the ESM, `.d.ts`, and `package.json` into `out_dir`.
    ///
    /// Files are named `index.mjs`, `index.d.ts`, and `package.json` — the APF
    /// layout for a single entry point.
    pub fn write_to(&self, out_dir: &Path) -> Result<Vec<std::path::PathBuf>, PackagrError> {
        std::fs::create_dir_all(out_dir).map_err(PackagrError::from)?;
        let esm = out_dir.join("index.mjs");
        let dts = out_dir.join("index.d.ts");
        let manifest = out_dir.join("package.json");
        std::fs::write(&esm, &self.esm).map_err(PackagrError::from)?;
        std::fs::write(&dts, &self.declarations).map_err(PackagrError::from)?;
        std::fs::write(&manifest, &self.manifest).map_err(PackagrError::from)?;
        Ok(vec![esm, dts, manifest])
    }
}

/// One compiled entry point within a packaged library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistEntry {
    /// The published sub-path (empty for the primary entry).
    pub sub_path: String,
    /// The directory, relative to the dist root, holding this entry's
    /// artifacts (`"."` for the primary, `"./testing"` for a secondary).
    pub dir: String,
    /// The entry's flattened (FESM) ESM module source — the entry's own private
    /// internal modules inlined into one file, with external/bare specifiers and
    /// sibling-entry references left as imports (see [`crate::fesm`]). This is the
    /// module emitted as the entry's `index.mjs`, i.e. what the published
    /// `package.json` `module`/`exports` point at. A single-file entry's flattened
    /// ESM is byte-identical to its compiled ESM.
    pub esm: String,
    /// The generated `.d.ts` source.
    pub declarations: String,
}

impl DistEntry {
    /// Whether this is the primary entry point.
    pub fn is_primary(&self) -> bool {
        self.sub_path.is_empty()
    }
}

/// The full result of packaging a library: the root APF `package.json`, every
/// compiled entry, and the copied asset names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistManifest {
    /// The published package name (the primary entry's name).
    pub name: String,
    /// The package version stamped into the manifest.
    pub version: String,
    /// The root APF `package.json`, pretty-printed JSON.
    pub manifest: String,
    /// Every compiled entry point (primary first).
    pub entries: Vec<DistEntry>,
    /// Asset file names copied verbatim into the dist root.
    pub assets: Vec<String>,
}

impl DistManifest {
    /// Write the whole dist tree under `dest`: the root `package.json`, then
    /// each entry's `index.mjs` + `index.d.ts` under its directory. The emitted
    /// `index.mjs` is the entry's flattened FESM module ([`DistEntry::esm`]) — the
    /// exact bytes the manifest's `module`/`exports` point at — so the per-entry
    /// layout is preserved while the module content is the flattened one.
    ///
    /// Returns every path written, in deterministic order.
    pub fn write_to(&self, dest: &Path) -> Result<Vec<std::path::PathBuf>, PackagrError> {
        std::fs::create_dir_all(dest).map_err(PackagrError::from)?;
        let mut written = Vec::new();

        let manifest_path = dest.join("package.json");
        std::fs::write(&manifest_path, &self.manifest).map_err(PackagrError::from)?;
        written.push(manifest_path);

        for entry in &self.entries {
            let dir = dest.join(entry.dir.trim_start_matches("./"));
            std::fs::create_dir_all(&dir).map_err(PackagrError::from)?;
            let esm = dir.join("index.mjs");
            let dts = dir.join("index.d.ts");
            std::fs::write(&esm, &entry.esm).map_err(PackagrError::from)?;
            std::fs::write(&dts, &entry.declarations).map_err(PackagrError::from)?;
            written.push(esm);
            written.push(dts);
        }
        Ok(written)
    }
}

/// Errors the packagr pipeline can surface.
#[derive(Debug)]
pub enum PackagrError {
    /// The package descriptor could not be parsed.
    Config(String),
    /// One or more compile diagnostics from the front-end.
    Compile(Vec<String>),
    /// Declaration emit (`oxc_isolated_declarations`) reported errors.
    Declaration(Vec<String>),
    /// An I/O failure while reading sources or writing artifacts.
    Io(std::io::Error),
}

impl std::fmt::Display for PackagrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackagrError::Config(msg) => write!(f, "config error: {msg}"),
            PackagrError::Compile(errs) => write!(f, "compile errors: {}", errs.join("; ")),
            PackagrError::Declaration(errs) => {
                write!(f, "declaration errors: {}", errs.join("; "))
            }
            PackagrError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for PackagrError {}

impl From<std::io::Error> for PackagrError {
    fn from(e: std::io::Error) -> Self {
        PackagrError::Io(e)
    }
}
