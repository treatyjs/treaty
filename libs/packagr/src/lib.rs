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
//! points, compiles/declares each, flattens each entry's internal modules into a
//! single APF FESM module ([`fesm`]), and returns a [`core::DistManifest`] (the
//! APF `package.json` plus per-entry artifacts).
//!
//! FESM flattening ([`fesm::flatten_entry_esm`]) inlines an entry's *private*
//! relative modules into one flat ES module while leaving bare specifiers
//! (`@angular/*`, `tslib`, npm deps) external and sibling published entries as
//! cross-entry references — the APF `fesm2022` shape, done with a self-contained
//! oxc-based inliner rather than a heavyweight bundler. A single-file component
//! entry has no inlinable modules, so flattening is a byte-for-byte identity.

pub mod apf;
pub mod compile;
pub mod component_dts;
pub mod config;
pub mod core;
pub mod css_optimizer;
pub mod dts;
pub mod dts_flatten;
pub mod fesm;
pub mod library;
pub mod stylesheet;

pub use config::{CompilationMode, PackageConfig, ResolvedEntry, SecondaryEntryConfig};
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
