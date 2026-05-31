//! Per-entry compilation to ESM.
//!
//! Reuses the committed front-ends directly: authoring extensions route through
//! [`rust_authoring::compile_file`]; anything else is treated as a bare
//! `@Component` TypeScript class and routed through
//! [`treaty_ivy::source_compile::compile_component_source`].

/// The ESM compile result for one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsmOutput {
    /// The compiled module source (empty when `errors` is non-empty).
    pub code: String,
    /// Compile diagnostics.
    pub errors: Vec<String>,
}

fn is_authoring(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".treaty")
        || lower.ends_with(".tsx")
        || lower.ends_with(".tjsx")
        || lower.ends_with(".ts")
}

/// Compile a single entry source to ESM via the appropriate real front-end.
pub fn compile_entry(source: &str, file_name: &str) -> EsmOutput {
    if is_authoring(file_name) {
        let compiled = rust_authoring::authoring::compile_file(source, file_name);
        EsmOutput {
            code: compiled.code,
            errors: compiled.errors,
        }
    } else {
        let compiled = treaty_ivy::source_compile::compile_component_source(source);
        EsmOutput {
            code: compiled.code,
            errors: compiled.errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ts_passes_through() {
        let out = compile_entry("export const x = 1;", "lib.ts");
        assert!(out.errors.is_empty());
        assert!(out.code.contains("export const x"));
    }
}
