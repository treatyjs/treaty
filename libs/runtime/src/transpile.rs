//! TypeScript -> JavaScript transpilation for the macro / server-function render-time path.
//!
//! Treaty macros (the top-of-file fenced block in a `.treaty` file) and TS server functions are
//! written in TypeScript, but the Nova engine only evaluates JavaScript. Before a macro or server
//! function can run in the isolate, its TypeScript-only syntax (type annotations, `interface` and
//! `type` declarations, `enum`/`namespace` lowering, parameter properties, `satisfies`/`as` casts,
//! non-null `!` assertions, etc.) must be removed.
//!
//! [`transpile_ts`] runs the source through the oxc pipeline used elsewhere in the workspace:
//! parse ([`oxc_parser`]) -> semantic analysis to obtain scoping ([`oxc_semantic`]) -> the
//! TypeScript transform ([`oxc_transformer`]) -> print JavaScript ([`oxc_codegen`]). Default
//! [`TransformOptions`] strip TypeScript syntax without down-levelling modern JavaScript, so the
//! emitted code stays in the feature subset Nova supports.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};

/// Failure to turn TypeScript source into runnable JavaScript.
///
/// Carries the concatenated oxc diagnostic messages. Surfaced to callers as a parse-class error
/// (the macro / server-fn source was not valid TypeScript, or used syntax the transform rejects).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranspileError(pub String);

impl std::fmt::Display for TranspileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transpile error: {}", self.0)
    }
}

impl std::error::Error for TranspileError {}

/// Transpile TypeScript `source` to JavaScript, stripping TypeScript-only syntax.
///
/// The returned string is plain JavaScript suitable for direct evaluation in the Nova isolate.
/// Source that is already plain JavaScript passes through unchanged in meaning (the round-trip is
/// parse -> print). A syntax error, or TypeScript that the transform cannot lower, yields
/// [`TranspileError`] with the collected diagnostics.
pub fn transpile_ts(source: &str) -> Result<String, TranspileError> {
    let allocator = Allocator::default();

    // Parse as TypeScript so the TS-only constructs are recognised by the parser and, in turn,
    // removed by the TypeScript transform below. `.ts` (not `.tsx`) keeps JSX disabled: macros and
    // server functions are scripts, not components, and leaving JSX off means a bare `<` is the
    // comparison/shift operator the author intended rather than the start of an element.
    let source_type = SourceType::default().with_typescript(true);
    let parsed = Parser::new(&allocator, source, source_type).parse();

    if !parsed.errors.is_empty() {
        return Err(TranspileError(join_messages(
            parsed.errors.iter().map(|e| e.to_string()),
        )));
    }

    let mut program = parsed.program;

    // Semantic analysis produces the scoping table the transformer threads through its passes.
    let semantic = SemanticBuilder::new().build(&program);
    if !semantic.errors.is_empty() {
        return Err(TranspileError(join_messages(
            semantic.errors.iter().map(|e| e.to_string()),
        )));
    }
    let scoping = semantic.semantic.into_scoping();

    // Default options strip TypeScript syntax while leaving the ECMAScript level untouched, so the
    // result stays in the modern-JS subset Nova accepts.
    let options = TransformOptions::default();
    let ret = Transformer::new(&allocator, Path::new("macro.ts"), &options)
        .build_with_scoping(scoping, &mut program);

    if !ret.errors.is_empty() {
        return Err(TranspileError(join_messages(
            ret.errors.iter().map(|e| e.to_string()),
        )));
    }

    Ok(Codegen::new().build(&program).code)
}

/// Join diagnostic messages into a single `; `-separated string, with a stable fallback when the
/// iterator is empty (oxc occasionally flags a failure without an attached message).
fn join_messages<I: Iterator<Item = String>>(messages: I) -> String {
    let joined = messages.collect::<Vec<_>>().join("; ");
    if joined.is_empty() {
        "unknown transpile failure".to_owned()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_type_annotations() {
        let js = transpile_ts("const x: number = 1; const y: string = 'a';").unwrap();
        assert!(!js.contains(": number"), "type annotation should be gone: {js}");
        assert!(!js.contains(": string"), "type annotation should be gone: {js}");
        assert!(js.contains("const x"));
        assert!(js.contains("const y"));
    }

    #[test]
    fn drops_interface_and_type_declarations() {
        let js = transpile_ts(
            "interface Foo { a: number } type Bar = string; const v = 1;",
        )
        .unwrap();
        assert!(!js.contains("interface"), "interface should be removed: {js}");
        assert!(!js.to_lowercase().contains("type bar"), "type alias should be removed: {js}");
        assert!(js.contains("const v"));
    }

    #[test]
    fn strips_function_parameter_and_return_types() {
        let js = transpile_ts("function add(a: number, b: number): number { return a + b; }")
            .unwrap();
        assert!(!js.contains(": number"), "param/return types should be gone: {js}");
        assert!(js.contains("function add"));
        assert!(js.contains("return a + b"));
    }

    #[test]
    fn strips_as_and_non_null_assertions() {
        let js = transpile_ts("const v = (foo as string)!; const n = bar!;").unwrap();
        assert!(!js.contains(" as "), "`as` cast should be gone: {js}");
        assert!(!js.contains("!;"), "non-null assertion should be gone: {js}");
    }

    #[test]
    fn passes_through_plain_javascript() {
        let js = transpile_ts("const obj = { a: 1, b: [2, 3] }; obj.a + obj.b[0];").unwrap();
        assert!(js.contains("const obj"));
        assert!(js.contains("a: 1"));
    }

    #[test]
    fn syntax_error_is_reported() {
        let err = transpile_ts("const = ;").expect_err("expected a transpile error");
        assert!(!err.0.is_empty());
    }
}
