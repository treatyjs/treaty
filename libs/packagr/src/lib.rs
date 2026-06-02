//! `treaty_packagr` — the Rust-native ng-packagr successor.
//!
//! Given a library's entry source files, packagr:
//!   1. compiles each entry to Ivy output via the committed front-ends
//!      ([`treaty_ivy`] for bare `@Component` classes, [`rust_authoring`] for
//!      authoring sources), producing per-entry ESM ([`compile`]);
//!   2. derives the entry's TypeScript declarations with
//!      [`oxc_isolated_declarations`] ([`dts`]);
//!   3. assembles an Angular Package Format (APF) `package.json` manifest
//!      describing the produced entry points ([`apf`]).
//!
//! The library-wide flow lives in [`library`]: [`library::package_library`]
//! reads a [`config::PackageConfig`], discovers the primary + secondary entry
//! points, compiles/declares each, and returns a [`core::DistManifest`] (the
//! APF `package.json` plus per-entry artifacts).
//!
//! What is intentionally NOT done yet, and is tracked as next work:
//!   * FESM flattening (one flattened ES module per entry) via `rolldown` —
//!     today each entry emits a single ESM file, which is APF-valid but not
//!     flattened across its internal modules.

pub mod apf;
pub mod compile;
pub mod component_dts;
pub mod config;
pub mod core;
pub mod dts;
pub mod library;

pub use config::{PackageConfig, ResolvedEntry, SecondaryEntryConfig};
pub use core::{DistEntry, DistManifest, EntryPoint, PackageOutput, PackagrError};
pub use library::{build_to_disk, package_library, package_library_at};

/// Run the full packagr pipeline for a single primary entry point.
///
/// Compiles `entry.source` to ESM + `.d.ts` and returns the produced artifacts
/// together with an APF manifest. Disk emission is the caller's responsibility
/// (see [`PackageOutput::write_to`]).
pub fn package(entry: &EntryPoint) -> Result<PackageOutput, PackagrError> {
    let esm = compile::compile_entry(&entry.source, &entry.file_name);
    if !esm.errors.is_empty() {
        return Err(PackagrError::Compile(esm.errors));
    }
    let declarations = dts::emit_dts_for_entry(&entry.source, &esm.code, &entry.file_name)?;
    let manifest = apf::manifest(entry);
    Ok(PackageOutput {
        name: entry.name.clone(),
        esm: esm.code,
        declarations,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_a_trivial_component() {
        let entry = EntryPoint {
            name: "@acme/widget".into(),
            file_name: "widget.ts".into(),
            source: "export const VERSION = '1.0.0';".into(),
        };
        // A non-Angular `.ts` is passed through faithfully by the authoring
        // front-end, so this should package without compile errors.
        let out = package(&entry).expect("packaging should succeed");
        assert!(out.manifest.contains("\"name\""));
        assert!(out.manifest.contains("@acme/widget"));
    }
}
