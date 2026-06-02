//! TypeScript declaration (`.d.ts`) emission.
//!
//! For sources authored as TypeScript (`.ts`/`.tsx`) we use
//! [`oxc_isolated_declarations`]: it derives a `.d.ts` purely from a single
//! file's syntax — no type-checker, no cross-file resolution — exactly the
//! model APF libraries are authored against (`"isolatedDeclarations": true`).
//!
//! Compiled Ivy ESM (the output of a `.treaty` SFC or other authoring source)
//! is *not* authored against isolated declarations — it is machine-generated
//! JavaScript with un-annotated exports — so isolated declarations rejects it.
//!
//! The dominant such case is an **Angular component**: a Treaty/JSX/`.treaty`
//! source lowers to a plain `function Button() { … }` with static `ɵfac`/`ɵcmp`
//! fields. The function has no return type, so isolated declarations fails with
//! `TS9007`. For these we reconstruct the *component class* `.d.ts` (typed
//! signal inputs + `ɵfac`/`ɵcmp` declarations) from the emitted Ivy metadata —
//! see [`crate::component_dts`] — exactly what `ngc`/`ng-packagr` would emit.
//!
//! For any other machine-generated module we synthesize a faithful declaration
//! from the compiled module's *export surface*: each export becomes a `declare`d
//! binding (typed as the precise Angular type where derivable, else `unknown`).
//! This keeps the published `.d.ts` resolvable by consumers without inventing
//! types.

use oxc_allocator::Allocator;
use oxc_ast::ast::{Declaration, ModuleExportName, Statement};
use oxc_codegen::Codegen;
use oxc_isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::component_dts;
use crate::core::PackagrError;

/// Emit the `.d.ts` source for a single entry.
///
/// Returns [`PackagrError::Declaration`] if the isolated-declarations transform
/// reports diagnostics (e.g. a missing explicit type annotation that isolated
/// declarations require).
pub fn emit_dts(source: &str, file_name: &str) -> Result<String, PackagrError> {
    // `SourceType::from_path` rejects authoring extensions (`.treaty`, `.tjsx`)
    // it does not know; in those cases the *raw* source is not TypeScript at
    // all and declarations are derived from the compiled output instead (see
    // [`emit_dts_for_entry`]). Treat the input here as TypeScript.
    let source_type = SourceType::from_path(file_name)
        .unwrap_or_else(|_| SourceType::default().with_typescript(true));
    emit_dts_with_type(source, source_type)
}

/// Choose the right declaration input for an entry.
///
/// The selection is driven by the *compiled* output, not the source extension:
///
/// 1. If the compiled ESM is an Angular **component** (a lowered function with
///    `ɵɵdefineComponent`), reconstruct its component-class `.d.ts` from the Ivy
///    metadata. This is the path every Treaty/JSX/`.treaty` component takes; it
///    sidesteps `TS9007` (the lowered function has no return-type annotation
///    that isolated declarations would require).
/// 2. Otherwise, plain TypeScript-syntax sources (`.ts`/`.tsx`) are declared
///    directly from their source via isolated declarations so explicit type
///    annotations survive (e.g. a `public-api.ts` of typed re-exports).
/// 3. Otherwise (authoring SFCs whose raw text is not TypeScript, or anything
///    isolated declarations rejects), synthesize a declaration from the compiled
///    module's export surface.
pub fn emit_dts_for_entry(
    source: &str,
    compiled: &str,
    file_name: &str,
) -> Result<String, PackagrError> {
    // (1) Angular component → reconstruct the component class.
    if let Some(dts) = component_dts::synthesize_component_dts(compiled) {
        return Ok(dts);
    }

    let lower = file_name.to_ascii_lowercase();
    let raw_is_typescript = lower.ends_with(".ts") || lower.ends_with(".tsx");
    if raw_is_typescript {
        // (2) Typed TS source — declare it directly; fall back to the compiled
        // export surface if isolated declarations rejects the raw source.
        return match emit_dts(source, file_name) {
            Ok(dts) => Ok(dts),
            Err(_) => synthesize_dts_from_exports(compiled),
        };
    }

    // (3) Authoring SFC output that is not a component.
    match emit_dts_with_type(compiled, SourceType::default().with_typescript(true)) {
        Ok(dts) => Ok(dts),
        Err(_) => synthesize_dts_from_exports(compiled),
    }
}

fn emit_dts_with_type(source: &str, source_type: SourceType) -> Result<String, PackagrError> {
    let allocator = Allocator::default();

    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        let msgs = parsed.errors.iter().map(|e| e.to_string()).collect();
        return Err(PackagrError::Declaration(msgs));
    }

    let id = IsolatedDeclarations::new(
        &allocator,
        IsolatedDeclarationsOptions {
            strip_internal: true,
        },
    );
    let ret = id.build(&parsed.program);

    if !ret.errors.is_empty() {
        let msgs = ret.errors.iter().map(|e| e.message.to_string()).collect();
        return Err(PackagrError::Declaration(msgs));
    }

    let printed = Codegen::new().build(&ret.program);
    Ok(printed.code)
}

/// Synthesize a `.d.ts` from the export surface of compiled module `source`.
///
/// Walks the module's top-level statements and emits one declaration per
/// export. The runtime types of machine-generated Ivy output are not knowable
/// from syntax alone, so named/default exports are declared as `unknown` — a
/// resolvable, non-lying surface. Re-exports (`export … from`) and
/// `export *`/`export default` are preserved structurally.
fn synthesize_dts_from_exports(source: &str) -> Result<String, PackagrError> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::default().with_typescript(true))
        .parse();
    if !parsed.errors.is_empty() {
        let msgs = parsed.errors.iter().map(|e| e.to_string()).collect();
        return Err(PackagrError::Declaration(msgs));
    }

    let mut decls: Vec<String> = Vec::new();
    let mut has_default = false;
    let mut named: Vec<String> = Vec::new();

    for stmt in &parsed.program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(src) = &export.source {
                    // `export { a, b } from './x'` — preserve the re-export.
                    let names: Vec<String> = export
                        .specifiers
                        .iter()
                        .map(|s| module_export_name(&s.exported))
                        .collect();
                    if export.specifiers.is_empty() {
                        decls.push(format!("export * from \"{}\";", src.value));
                    } else {
                        decls.push(format!(
                            "export {{ {} }} from \"{}\";",
                            names.join(", "),
                            src.value
                        ));
                    }
                } else if let Some(decl) = &export.declaration {
                    // `export const/function/class X` — declare the binding(s).
                    for name in declaration_names(decl) {
                        named.push(name);
                    }
                } else {
                    // `export { a, b }` referencing local bindings.
                    for spec in &export.specifiers {
                        named.push(module_export_name(&spec.exported));
                    }
                }
            }
            Statement::ExportDefaultDeclaration(_) => {
                has_default = true;
            }
            Statement::ExportAllDeclaration(export) => {
                if let Some(alias) = &export.exported {
                    decls.push(format!(
                        "export * as {} from \"{}\";",
                        module_export_name(alias),
                        export.source.value
                    ));
                } else {
                    decls.push(format!("export * from \"{}\";", export.source.value));
                }
            }
            _ => {}
        }
    }

    named.sort();
    named.dedup();
    for name in &named {
        decls.push(format!("export declare const {name}: unknown;"));
    }
    if has_default {
        decls.push("declare const _default: unknown;".to_string());
        decls.push("export default _default;".to_string());
    }

    // An entry that exports nothing still needs a valid (empty) module.
    if decls.is_empty() {
        decls.push("export {};".to_string());
    }

    let mut out = decls.join("\n");
    out.push('\n');
    Ok(out)
}

/// The string form of a module export name (identifier or string literal).
fn module_export_name(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.to_string(),
        ModuleExportName::IdentifierReference(id) => id.name.to_string(),
        ModuleExportName::StringLiteral(lit) => lit.value.to_string(),
    }
}

/// The bound names introduced by an exported declaration.
fn declaration_names(decl: &Declaration) -> Vec<String> {
    match decl {
        Declaration::VariableDeclaration(var) => var
            .declarations
            .iter()
            .filter_map(|d| d.id.get_identifier_name().map(|n| n.to_string()))
            .collect(),
        Declaration::FunctionDeclaration(func) => {
            func.id.as_ref().map(|i| vec![i.name.to_string()]).unwrap_or_default()
        }
        Declaration::ClassDeclaration(class) => {
            class.id.as_ref().map(|i| vec![i.name.to_string()]).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_declaration_for_typed_export() {
        let dts = emit_dts("export const VERSION: string = '1.0.0';", "lib.ts")
            .expect("isolated declarations should succeed for a typed export");
        assert!(dts.contains("VERSION"));
        assert!(dts.contains("string"));
    }

    #[test]
    fn synthesizes_from_compiled_ivy_default_export() {
        // A minimal stand-in for compiled Ivy output: a function component the
        // SFC pipeline default-exports, which isolated declarations rejects.
        let compiled = "function Greeting() { return {}; }\nGreeting.x = 1;\nexport default Greeting;";
        let dts = emit_dts_for_entry("", compiled, "greeting.treaty")
            .expect("should synthesize from export surface");
        assert!(dts.contains("export default"));
    }

    #[test]
    fn synthesizes_named_exports() {
        let compiled = "const a = 1; const b = 2;\nexport { a, b };";
        let dts = synthesize_dts_from_exports(compiled).unwrap();
        assert!(dts.contains("export declare const a: unknown;"));
        assert!(dts.contains("export declare const b: unknown;"));
    }
}
