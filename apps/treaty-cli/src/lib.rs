//! `treaty_cli` — the library powering the Treaty Rust CLI.
//!
//! The Rust-native successor to the TypeScript `@treaty/cli`. It calls the
//! committed `render3` Ivy compiler and the `rust_authoring` front-ends directly
//! (no NAPI), and orchestrates bundling through a pluggable [`bundler`] backend.
//!
//! The crate is split so the `treaty` binary (`src/main.rs`) is a thin `clap`
//! adapter over this reusable library. The two extension points are:
//!
//!   * [`plugin::CliPlugin`] + [`plugin::PluginRegistry`] — the subcommand
//!     plugin system.
//!   * [`bundler::BundlerBackend`] — dev/build over a configured bundler, with an
//!     external-tool backend (rspack/rsbuild/vite) and a Rust-native fallback.
//!
//! plus the federation CI building blocks:
//!
//!   * [`affected`] — the affected-module graph computation (shared-lib fan-out).
//!   * [`deploy::DeployPlugin`] — the pluggable deploy/rollback layer.

pub mod affected;
pub mod bundler;
pub mod compile;
pub mod config;
pub mod core;
pub mod deploy;
pub mod generate;
pub mod native_build;
pub mod plugin;
pub mod resolve;
pub mod selectors;
pub mod serve;
pub mod transform;
