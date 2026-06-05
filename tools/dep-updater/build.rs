//! Embeds an `asInvoker` application manifest into every binary this crate
//! links on the MSVC toolchain.
//!
//! Windows' UAC installer-detection heuristic forces elevation on executables
//! whose names contain tokens like `update`/`updater`/`setup`/`install`. Both
//! this crate's binaries (`dep-updater`) and its Cargo test harnesses
//! (`dep_updater-<hash>.exe`, `dep-updater-<hash>.exe`) match that heuristic,
//! so running them — including `cargo test` — fails with `os error 740` ("The
//! requested operation requires elevation"). Embedding a manifest that
//! explicitly requests `asInvoker` opts the binaries out of installer
//! detection, restoring non-elevated execution.
//!
//! This applies the linker args unconditionally for every target the crate
//! produces (bins + test/bench harnesses), which is exactly the set affected.
//! It is a no-op on non-MSVC hosts.

use std::path::Path;

fn main() {
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "msvc" {
        // Only the MSVC linker understands /MANIFEST:EMBED.
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let manifest = Path::new(&manifest_dir).join("dep-updater.manifest");
    println!("cargo:rerun-if-changed=dep-updater.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    // Embed the manifest into the linked PE so Windows treats the binary as
    // asInvoker rather than auto-elevating it as an installer.
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
