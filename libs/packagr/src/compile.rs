//! Per-entry compilation to ESM.
//!
//! Reuses the committed front-ends directly: authoring extensions route through
//! [`rust_authoring::compile_file`]; anything else is treated as a bare
//! `@Component` TypeScript class and routed through
//! [`treaty_ivy::source_compile::compile_component_source`].
//!
//! A plain-TypeScript `@Component` entry may carry external stylesheets
//! (`styleUrls` / `styleUrl`). Those reference files on disk and, for SCSS/Sass,
//! need preprocessing before the compiler can fold + scope them — exactly the
//! pre-`ngc` step ng-packagr runs. [`compile_entry_at`] resolves and preprocesses
//! those files (via [`crate::stylesheet`]) and threads them through the
//! compiler's host-resolution channel
//! ([`treaty_ivy::source_compile::compile_component_source_with_resolved`]) so a
//! `styleUrls` component emits the IDENTICAL scoped `styles: [...]` array as the
//! inline-`styles` equivalent.

use std::path::Path;

use crate::stylesheet;

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
///
/// This path has no access to the entry's on-disk location, so a `@Component`
/// with external `styleUrls` cannot be resolved here (its files live on disk);
/// only inline `styles: [...]` are folded. Use [`compile_entry_at`] when the
/// entry's source path is known (the library build path) so external stylesheets
/// are resolved + preprocessed.
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

/// Compile a single entry whose on-disk `source_path` is known.
///
/// Identical to [`compile_entry`] for authoring sources and for plain-TS entries
/// with no external stylesheets — and BYTE-IDENTICAL for those, since it takes the
/// exact same front-end calls. For a plain-TS `@Component` entry that declares
/// `styleUrls` / `styleUrl`, it first resolves + preprocesses those files relative
/// to the source directory ([`stylesheet::resolve_component_styles`]) and feeds the
/// resulting CSS through the compiler's host-resolution channel, so the emitted
/// (scoped) `styles: [...]` matches ng-packagr's.
pub fn compile_entry_at(source: &str, source_path: &Path) -> EsmOutput {
    compile_entry_at_mode(source, source_path, false)
}

/// Like [`compile_entry_at`], but with the `compilationMode: "partial"` flag.
///
/// When `partial` is `true`, a plain-TypeScript `@Component`/`@Directive` entry is compiled to its
/// `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective` PARTIAL declaration (the source front-end emits it
/// from the metadata + the original template string). The DI/pipe family stays AOT here and is
/// inverted by the caller's `treaty_ivy::emit_partial` span-rewrite pass. When `partial` is `false`
/// the emit is byte-identical to [`compile_entry`] — the default Full/AOT path.
///
/// NOTE: the partial flag is honoured on the plain-TS inline-style front-end only. The
/// external-`styleUrls` resolution path and the authoring (`.treaty`/`.tsx`) front-ends still emit
/// AOT (their partial support is a follow-up); the caller's `emit_partial` pass then partials their
/// DI/pipe family, leaving any component/directive def as AOT (a valid, loadable mix).
pub fn compile_entry_at_mode(source: &str, source_path: &Path, partial: bool) -> EsmOutput {
    let file_name = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("index.ts");

    // A `.ts` entry whose `@Component` declares external `styleUrls`/`styleUrl` needs those files
    // resolved + preprocessed (the pre-`ngc` step ng-packagr runs) before the SAME base-Angular
    // `.ts` front-end can fold + scope them. Detect that case and route through the
    // resolution-aware front-end entry; everything else takes the EXACT same call as
    // `compile_entry` (byte-identical).
    let lower = file_name.to_ascii_lowercase();
    let is_plain_ts = lower.ends_with(".ts");
    if is_plain_ts && stylesheet::has_external_styles(source) {
        let source_dir = source_path.parent().unwrap_or_else(|| Path::new("."));
        // Style-preprocess diagnostics (an unsupported-preprocessor passthrough, or a SCSS error
        // that fell back to raw text) are NON-FATAL: the build proceeds with a best-effort
        // stylesheet rather than aborting, matching packagr's lenient asset/style handling. They
        // are intentionally not surfaced as compile `errors` (which would fail the entry).
        let (resolved, _style_diags) = stylesheet::resolve_component_styles(source, source_dir);
        let compiled =
            rust_authoring::angular_source::compile_angular_source_with_resolved(source, file_name, &resolved);
        return EsmOutput {
            code: compiled.code,
            errors: compiled.errors,
        };
    }

    // PARTIAL mode, plain-TS inline-style `@Component`/`@Directive` (the dominant library-publish
    // shape, and the one the partial component/directive declaration emit targets): route through
    // the option-carrying front-end so it emits `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective`. The
    // Full (`partial == false`) path below is left COMPLETELY unchanged — byte-identical to before.
    if partial && is_plain_ts {
        let opts = treaty_ivy::source_compile::CompileOptions {
            emit_partial_component: true,
            ..Default::default()
        };
        let compiled =
            treaty_ivy::source_compile::compile_component_source_with_options(source, opts);
        return EsmOutput {
            code: compiled.code,
            errors: compiled.errors,
        };
    }

    // Authoring sources (and, in Full mode, plain `.ts`), take the identical front-end call
    // `compile_entry` uses.
    if is_authoring(file_name) {
        let compiled = rust_authoring::authoring::compile_file(source, file_name);
        return EsmOutput {
            code: compiled.code,
            errors: compiled.errors,
        };
    }
    let compiled = treaty_ivy::source_compile::compile_component_source(source);
    EsmOutput {
        code: compiled.code,
        errors: compiled.errors,
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
