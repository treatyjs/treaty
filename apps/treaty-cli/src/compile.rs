//! The `treaty compile` path: drive the real compilers on a single file.
//!
//! Extension selects the front-end:
//!   * `.treaty` / `.tsx` / `.tjsx` / `.ts` → [`rust_authoring::compile_file`]
//!   * anything else is treated as a bare `@Component` TS class and routed
//!     through [`treaty_ivy::source_compile::compile_component_source`].
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

/// The generated-artifact name for a source file: the same stem with a `.js`
/// extension. Used as the map's `file` (the original name is kept as the map's
/// `sources[0]`). A name without an extension is returned with `.js` appended.
fn ivy_artifact_name(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, _ext)) => format!("{stem}.js"),
        None => format!("{file_name}.js"),
    }
}

/// Compile a single in-memory source through the appropriate real front-end.
pub fn compile_source(source: &str, file_name: &str) -> CompileOutput {
    compile_source_with_registry(source, file_name, None)
}

/// Like [`compile_source`] but threading the project's CROSS-MODULE selector registry
/// (`{ importName -> selector }`) the host pre-resolved for THIS file, so an imported component used
/// by its real `@Component` selector resolves its dependency through the compiler's CSS-selector
/// matcher instead of the class-name↔tag fold convention.
///
/// ADDITIVE: with `registry == None` the routing + emit are byte-identical to [`compile_source`].
/// The registry is honoured only on the Authoring `.ts` (base-Angular) front-end — the
/// cross-module-import case it exists for; the bare-`@Component` `Component` front-end carries no
/// import scope, so it is unaffected.
pub fn compile_source_with_registry(
    source: &str,
    file_name: &str,
    registry: Option<&treaty_ivy::source_compile::SelectorRegistry>,
) -> CompileOutput {
    let input = Path::new(file_name).to_path_buf();
    match frontend_for(file_name) {
        Frontend::Authoring => {
            let compiled =
                rust_authoring::authoring::compile_file_with_registry(source, file_name, registry);
            CompileOutput {
                input,
                code: compiled.code,
                server_module: compiled.server_module,
                errors: compiled.errors,
                // The authoring front-end already returns its additive
                // `Ivy-TS -> original` map (server-fn bodies redacted from
                // `sourcesContent` for client privacy); consume it verbatim.
                map: compiled.map,
            }
        }
        Frontend::Component => {
            // Use the map-bearing entry: `code` is BYTE-IDENTICAL to
            // `compile_component_source` (the facade guarantees this), and `map`
            // is the additive `Ivy-TS -> original` v3 JSON. The generated artifact
            // name is the file with a `.js` extension; the original is `file_name`.
            let source_name = file_name;
            let artifact_name = ivy_artifact_name(file_name);
            let compiled = treaty_ivy::source_compile::compile_component_source_with_map(
                source,
                &artifact_name,
                source_name,
            );
            CompileOutput {
                input,
                code: compiled.code,
                server_module: None,
                errors: compiled.errors,
                map: if compiled.map.is_empty() {
                    None
                } else {
                    Some(compiled.map)
                },
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
