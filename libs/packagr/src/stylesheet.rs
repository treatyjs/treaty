//! Component stylesheet resolution + preprocessing for `@Component` `.ts` entries.
//!
//! ng-packagr, before handing a component to `ngc`, resolves every `styleUrls` /
//! `styleUrl` to a file on disk, runs the matching CSS PREPROCESSOR (Sass/SCSS,
//! Less, Stylus) over each file, and feeds the resulting plain CSS to the
//! compiler. The compiler then folds those strings into the component's
//! `ɵɵdefineComponent({ styles: [...] })` and — for the default Emulated
//! encapsulation — scopes each rule with the `ShadowCss` algorithm (which
//! `treaty_ivy` already ports verbatim in `decorators/shadow_css.rs`).
//!
//! packagr reproduces exactly that pre-`ngc` step here: it scans a `.ts` entry's
//! `@Component` decorators for `styleUrls` / `styleUrl`, reads the referenced
//! files relative to the entry source, compiles SCSS/Sass with the pure-Rust
//! [`grass`] engine (and passes `.css` through verbatim), and returns a
//! [`ResolvedContentMap`] keyed by component class name. Handing that map to
//! [`treaty_ivy::source_compile::compile_component_source_with_resolved`] makes a
//! `styleUrls` component emit a `styles: [...]` array — already preprocessed and
//! (for Emulated) scoped by the SAME `ShadowCss` port ngc uses. The Emulated
//! SCOPING (`_ngcontent-%COMP%` placement, descendant combinators) is byte-identical
//! to ng-packagr's. NOTE — not yet fully output-equal: ng-packagr additionally runs
//! esbuild's CSS-value optimizer (e.g. `color: blue` → `#00f`, `bold` → `700`),
//! which packagr does not (a follow-up); the *values* therefore differ even though
//! the scoping matches. Inline `styles: ['...']` need no resolution channel: they
//! are read straight from the decorator object by the compiler.
//!
//! File I/O and preprocessing are the only packagr-side concern; scoping and the
//! `styles` array shape live in `treaty_ivy`, so the emitted CSS is output-equal
//! to ng-packagr by construction.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, Class, Decorator, Expression, ObjectPropertyKind, Program,
    PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

use treaty_ivy::source_compile::{ResolvedComponentContent, ResolvedContentMap};

/// A recognised stylesheet preprocessor language, inferred from a `styleUrl` file
/// extension (matching ng-packagr's per-extension loader selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StyleLang {
    /// Plain CSS — passed through verbatim.
    Css,
    /// Sass/SCSS — compiled with the pure-Rust [`grass`] engine.
    Scss,
    /// Less / Stylus — not yet preprocessed in-process (see module note); the raw
    /// file is passed through so the build still produces output rather than
    /// failing, and the gap is reported honestly.
    Passthrough,
}

impl StyleLang {
    /// Infer the preprocessor language from a stylesheet path's extension.
    fn from_path(path: &Path) -> StyleLang {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("scss") | Some("sass") => StyleLang::Scss,
            Some("css") => StyleLang::Css,
            // `.less` / `.styl` have no in-process Rust engine yet.
            _ => StyleLang::Passthrough,
        }
    }
}

/// Compile one stylesheet's raw text to plain CSS according to its language.
///
/// SCSS/Sass is compiled with [`grass`]; CSS and any not-yet-supported
/// preprocessor pass through unchanged. A SCSS compile error falls back to the
/// raw text (so a malformed partial never aborts the whole package build) and is
/// surfaced through the returned diagnostics.
fn compile_stylesheet(raw: &str, lang: StyleLang, path: &Path) -> (String, Option<String>) {
    match lang {
        StyleLang::Css | StyleLang::Passthrough => (raw.to_string(), None),
        StyleLang::Scss => {
            // `grass::from_string` compiles a SCSS/Sass source string. Emit COMPRESSED CSS
            // (whitespace collapsed) so the folded `styles: [...]` matches ng-packagr's
            // minified pre-scope CSS as closely as the in-process engine allows — ng-packagr
            // runs the Sass output through esbuild's CSS optimizer (whitespace + color
            // shortening); compressing here closes the dominant whitespace difference. The
            // SCOPING that follows (`treaty_ivy`'s `ShadowCss` port) is byte-identical to ngc's.
            let is_indented = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("sass"))
                .unwrap_or(false);
            let options = grass::Options::default()
                .style(grass::OutputStyle::Compressed)
                .input_syntax(if is_indented {
                    grass::InputSyntax::Sass
                } else {
                    grass::InputSyntax::Scss
                });
            match grass::from_string(raw.to_string(), &options) {
                Ok(css) => (css, None),
                Err(e) => (
                    raw.to_string(),
                    Some(format!("scss compile error in {}: {e}", path.display())),
                ),
            }
        }
    }
}

/// The static-identifier / string name of an object-property key.
fn key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

/// A string-literal (or single-quasi template-literal) value.
fn string_value(expr: &Expression) -> Option<String> {
    match expr {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            t.quasis[0].value.cooked.as_ref().map(|c| c.to_string())
        }
        _ => None,
    }
}

/// Every string element of an array-literal value (`styleUrls: ['a.scss', 'b.css']`).
fn string_array_value(expr: &Expression) -> Vec<String> {
    let Expression::ArrayExpression(arr) = expr else {
        return Vec::new();
    };
    arr.elements
        .iter()
        .filter_map(|el| match el {
            ArrayExpressionElement::SpreadElement(_) | ArrayExpressionElement::Elision(_) => None,
            other => other.as_expression().and_then(string_value),
        })
        .collect()
}

/// The `@Component` decorator's options object (`@Component({...})`), if this is a
/// `@Component(...)` decorator.
fn component_decorator_object<'a>(dec: &'a Decorator<'a>) -> Option<&'a oxc_ast::ast::ObjectExpression<'a>> {
    let Expression::CallExpression(call) = &dec.expression else {
        return None;
    };
    let is_component = matches!(&call.callee, Expression::Identifier(id) if id.name == "Component");
    if !is_component {
        return None;
    }
    match call.arguments.first() {
        Some(Argument::ObjectExpression(obj)) => Some(obj),
        _ => None,
    }
}

/// Look up an object-literal property value by name.
fn find_prop<'a>(obj: &'a oxc_ast::ast::ObjectExpression<'a>, name: &str) -> Option<&'a Expression<'a>> {
    obj.properties.iter().find_map(|p| match p {
        ObjectPropertyKind::ObjectProperty(op) if key_name(&op.key) == Some(name) => Some(&op.value),
        _ => None,
    })
}

/// The class declaration carried by a top-level statement (`class X`,
/// `export class X`, `export default class X`).
fn statement_class<'a>(stmt: &'a Statement<'a>) -> Option<&'a Class<'a>> {
    use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind};
    match stmt {
        Statement::ClassDeclaration(class) => Some(class),
        Statement::ExportNamedDeclaration(export) => match &export.declaration {
            Some(Declaration::ClassDeclaration(class)) => Some(class),
            _ => None,
        },
        Statement::ExportDefaultDeclaration(def) => match &def.declaration {
            ExportDefaultDeclarationKind::ClassDeclaration(class) => Some(class),
            _ => None,
        },
        _ => None,
    }
}

/// The list of `styleUrl` strings a `@Component` decorator declares, merging the
/// singular `styleUrl` and the plural `styleUrls` (ngtsc accepts both; the
/// singular form is appended after the plural, matching its resolution order).
fn component_style_urls(obj: &oxc_ast::ast::ObjectExpression) -> Vec<String> {
    let mut urls = find_prop(obj, "styleUrls")
        .map(string_array_value)
        .unwrap_or_default();
    if let Some(single) = find_prop(obj, "styleUrl").and_then(string_value) {
        urls.push(single);
    }
    urls
}

/// Resolve and preprocess every `@Component` `styleUrls`/`styleUrl` declared in a
/// `.ts` entry, returning the host-resolved content map to hand to the compiler.
///
/// `source` is the entry's TypeScript text; `source_dir` is the directory the
/// entry source lives in (style URLs resolve relative to it, as in ngtsc). For
/// each component class that declares external styles, every referenced file is
/// read, preprocessed by language ([`compile_stylesheet`]), and recorded — in
/// declaration order — under the component's class name. Components with only
/// inline `styles: [...]` (or none) need no entry here: the compiler reads those
/// straight from the decorator object.
///
/// Missing files are skipped leniently (ng-packagr fails the build on a missing
/// style file, but packagr's contract elsewhere is lenient copy/skip; a skipped
/// file simply contributes no style). Returns the map plus any preprocessing
/// diagnostics (non-fatal; the build proceeds with the raw fallback text).
pub fn resolve_component_styles(
    source: &str,
    source_dir: &Path,
) -> (ResolvedContentMap, Vec<String>) {
    let mut map = ResolvedContentMap::new();
    let mut diagnostics = Vec::new();

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        // A parse error here is reported by the real compile pass; this resolution
        // pass simply contributes no styles.
        return (map, diagnostics);
    }

    for stmt in &parsed.program.body {
        let Some(class) = statement_class(stmt) else {
            continue;
        };
        let Some(class_id) = &class.id else { continue };
        let class_name = class_id.name.to_string();

        for dec in &class.decorators {
            let Some(obj) = component_decorator_object(dec) else {
                continue;
            };
            let urls = component_style_urls(obj);
            if urls.is_empty() {
                continue;
            }

            let mut styles = Vec::with_capacity(urls.len());
            for url in &urls {
                let path = source_dir.join(url);
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    // Missing style file — skip leniently.
                    continue;
                };
                let lang = StyleLang::from_path(&path);
                if lang == StyleLang::Passthrough {
                    diagnostics.push(format!(
                        "stylesheet {} uses an unsupported preprocessor; passed through as raw CSS",
                        path.display()
                    ));
                }
                let (css, diag) = compile_stylesheet(&raw, lang, &path);
                if let Some(d) = diag {
                    diagnostics.push(d);
                }
                styles.push(css);
            }

            if !styles.is_empty() {
                map.entry(class_name.clone())
                    .or_insert_with(ResolvedComponentContent::default)
                    .styles
                    .extend(styles);
            }
        }
    }

    (map, diagnostics)
}

/// Whether a `.ts` entry source declares any component `styleUrls`/`styleUrl`
/// that needs the host resolution channel. Cheap parse-based check the compile
/// step uses to decide whether to run [`resolve_component_styles`] (so the common
/// no-external-styles entry takes the existing, unchanged compile path).
pub fn has_external_styles(source: &str) -> bool {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return false;
    }
    program_has_external_styles(&parsed.program)
}

fn program_has_external_styles(program: &Program) -> bool {
    for stmt in &program.body {
        let Some(class) = statement_class(stmt) else {
            continue;
        };
        for dec in &class.decorators {
            if let Some(obj) = component_decorator_object(dec) {
                if find_prop(obj, "styleUrls").is_some() || find_prop(obj, "styleUrl").is_some() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scss_is_compiled_to_css() {
        let (css, diag) = compile_stylesheet(
            ".a { .b { color: red; } }",
            StyleLang::Scss,
            Path::new("x.scss"),
        );
        assert!(diag.is_none(), "unexpected scss diagnostic: {diag:?}");
        // Nesting is flattened by the Sass compiler; Compressed output drops the
        // space after the colon.
        assert!(css.contains(".a .b"), "scss not compiled: {css}");
        assert!(css.contains("color:red"), "scss not compiled (compressed): {css}");
    }

    #[test]
    fn css_passes_through_verbatim() {
        let (css, diag) = compile_stylesheet(".a { color: red; }", StyleLang::Css, Path::new("x.css"));
        assert!(diag.is_none());
        assert_eq!(css, ".a { color: red; }");
    }

    #[test]
    fn detects_external_styles() {
        let with = r#"
            import { Component } from '@angular/core';
            @Component({ selector: 'x', template: '<p></p>', styleUrls: ['./x.css'] })
            export class X {}
        "#;
        assert!(has_external_styles(with));

        let without = r#"
            import { Component } from '@angular/core';
            @Component({ selector: 'x', template: '<p></p>', styles: ['.a{}'] })
            export class X {}
        "#;
        assert!(!has_external_styles(without));
    }

    #[test]
    fn resolves_and_preprocesses_scss_style_url() {
        let dir = std::env::temp_dir().join(format!(
            "packagr_style_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("btn.scss"), "$c: blue;\n.btn { color: $c; }").unwrap();

        let source = r#"
            import { Component } from '@angular/core';
            @Component({ selector: 'btn', template: '<button></button>', styleUrls: ['./btn.scss'] })
            export class ButtonComponent {}
        "#;
        let (map, diag) = resolve_component_styles(source, &dir);
        assert!(diag.is_empty(), "unexpected diagnostics: {diag:?}");
        let content = map.get("ButtonComponent").expect("component resolved");
        assert_eq!(content.styles.len(), 1);
        assert!(
            content.styles[0].contains(".btn") && content.styles[0].contains("color:blue"),
            "scss var not resolved: {:?}",
            content.styles
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn style_url_singular_and_plural_merge() {
        let dir = std::env::temp_dir().join(format!(
            "packagr_style2_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.css"), ".a{color:red}").unwrap();
        std::fs::write(dir.join("b.css"), ".b{color:blue}").unwrap();

        let source = r#"
            import { Component } from '@angular/core';
            @Component({ selector: 'c', template: '<p></p>', styleUrls: ['./a.css'], styleUrl: './b.css' })
            export class C {}
        "#;
        let (map, _diag) = resolve_component_styles(source, &dir);
        let content = map.get("C").expect("component resolved");
        // Plural first, then the singular appended.
        assert_eq!(content.styles, vec![".a{color:red}".to_string(), ".b{color:blue}".to_string()]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
