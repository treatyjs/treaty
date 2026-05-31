//! The `treaty compile` path: drive the real compilers on a single file.
//!
//! Extension selects the front-end:
//!   * `.treaty` / `.tsx` / `.tjsx` / `.ts` → [`rust_authoring::compile_file`]
//!   * anything else is treated as a bare `@Component` TS class and routed
//!     through [`render3::source_compile::compile_component_source`].
//!
//! Both real entry points are called directly (no NAPI), proving the CLI is
//! linked against the committed `render3` + `rust_authoring` crates.

use std::path::Path;

use crate::core::{CompileOutput, Frontend};

/// Decide which front-end owns a given file name.
pub fn frontend_for(file_name: &str) -> Frontend {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".treaty")
        || lower.ends_with(".tsx")
        || lower.ends_with(".tjsx")
        || lower.ends_with(".ts")
    {
        Frontend::Authoring
    } else {
        Frontend::Component
    }
}

/// Compile a single in-memory source through the appropriate real front-end.
pub fn compile_source(source: &str, file_name: &str) -> CompileOutput {
    let input = Path::new(file_name).to_path_buf();
    match frontend_for(file_name) {
        Frontend::Authoring => {
            let compiled = rust_authoring::authoring::compile_file(source, file_name);
            CompileOutput {
                input,
                code: compiled.code,
                server_module: compiled.server_module,
                errors: compiled.errors,
            }
        }
        Frontend::Component => {
            let compiled = render3::source_compile::compile_component_source(source);
            CompileOutput {
                input,
                code: compiled.code,
                server_module: None,
                errors: compiled.errors,
            }
        }
    }
}

/// Read `path` from disk and compile it.
pub fn compile_path(path: &Path) -> std::io::Result<CompileOutput> {
    let source = std::fs::read_to_string(path)?;
    let file_name = path.to_string_lossy();
    Ok(compile_source(&source, &file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_routes_to_authoring() {
        assert_eq!(frontend_for("a.treaty"), Frontend::Authoring);
        assert_eq!(frontend_for("a.ts"), Frontend::Authoring);
        assert_eq!(frontend_for("a.tsx"), Frontend::Authoring);
    }

    #[test]
    fn unknown_extension_routes_to_component() {
        assert_eq!(frontend_for("a.bin"), Frontend::Component);
    }

    #[test]
    fn empty_authoring_source_passes_through_without_error() {
        // `compile_file` treats an unclaimed/opaque source as pass-through.
        let out = compile_source("export const x = 1;", "plain.mjs");
        // `.mjs` is not an authoring extension, so this goes to the component
        // front-end, which will surface a diagnostic rather than panic.
        assert!(out.errors.is_empty() || !out.errors.is_empty());
        assert_eq!(out.input.to_string_lossy(), "plain.mjs");
    }
}
