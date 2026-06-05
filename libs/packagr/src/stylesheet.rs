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
//! [`ResolvedContentMap`] keyed by component class name.
//!
//! **Less / Stylus** are an HONEST passthrough: no mature pure-Rust Less or Stylus
//! compiler exists (surveyed — `lightningcss` is a CSS minifier, not a Less/Stylus
//! preprocessor; the real compilers are JS-only, so ng-packagr shells out to the npm
//! `less`/`stylus` packages). packagr passes the raw `.less`/`.styl` text through with a
//! diagnostic rather than failing the build. SCSS/Sass, by contrast, fully compile via
//! grass — so the supported preprocessor set is **SCSS/Sass + plain CSS**. Handing that map to
//! [`treaty_ivy::source_compile::compile_component_source_with_resolved`] makes a
//! `styleUrls` component emit a `styles: [...]` array — already preprocessed and
//! (for Emulated) scoped by the SAME `ShadowCss` port ngc uses. The Emulated
//! SCOPING (`_ngcontent-%COMP%` placement, descendant combinators) is byte-identical
//! to ng-packagr's.
//!
//! ## CSS minification toward the ng-packagr@21 FESM
//!
//! ng-packagr's real pipeline runs esbuild's CSS minifier BEFORE ngc's `ShadowCss`
//! scoping (`encapsulateStyle(esbuildBundleFile(css))`) and then emits the FESM.
//! packagr's pipeline is the REVERSE — treaty_ivy's `ShadowCss` port scopes first, then
//! [`optimize_compiled_styles`] post-processes every `styles: [...]` string in the
//! COMPILED module (covering both inline `styles` and resolved `styleUrls` uniformly).
//! The optimizer therefore reproduces exactly those esbuild transforms that SURVIVE
//! scoping, targeting the ng-packagr@21 **FESM** `styles` bytes as the oracle (NOT a raw
//! `esbuild.transformSync` — the two differ; e.g. the FESM tightens `@media` preludes
//! while a bare esbuild transform and `encapsulateStyle(esbuildBundleFile())` both leave
//! them loose). See [`crate::css_optimizer`] for the full pipeline-order analysis and the
//! per-construct fidelity boundary.
//!
//! File I/O and preprocessing are the only packagr-side concern; scoping and the
//! `styles` array shape live in `treaty_ivy`. The emitted CSS is **byte-identical** to a
//! live ng-packagr@21 build for the whole css-oracle fixture — scoping, named-color→hex,
//! `bold`→`700`, hex shortening, `0px`→`0`, whitespace collapse, the selector-list
//! comma+space, `@media` prelude tightening, `@keyframes` `from`→`0%` /
//! `rotate(0deg)`→`rotate(0)`, custom-property verbatim, `!important` tightening,
//! attribute-quote stripping, and legacy `::before`→`:before` (all regression-locked in
//! [`crate::css_optimizer`]). The remaining fidelity boundary is the deep typed-value
//! model esbuild only applies inside shorthands (shorthand-internal color conversion,
//! `rgb()`/`hsl()` function conversion) — deliberately not done, to never over-minify.

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
    /// Less / Stylus (and any other non-CSS preprocessor) — passed through as raw text.
    ///
    /// There is no mature pure-Rust Less or Stylus compiler (surveyed: `lightningcss` parses/minifies
    /// CSS but does NOT implement the Less/Stylus *preprocessor* languages — variables, mixins,
    /// functions; the `less` and `stylus` compilers are JS-only, which is why ng-packagr shells out to
    /// the npm `less`/`stylus` packages). Adding one would mean porting an entire preprocessor, so
    /// packagr does NOT preprocess Less/Stylus in-process. The raw file is passed through so the build
    /// still produces output rather than failing, the variable/mixin syntax is left intact, and the
    /// gap is reported honestly via a diagnostic. (SCSS/Sass, by contrast, fully compile via grass.)
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
            // `.less` / `.styl` have no in-process Rust engine (no pure-Rust Less/Stylus compiler
            // exists — see `StyleLang::Passthrough`); pass them through with a diagnostic.
            _ => StyleLang::Passthrough,
        }
    }

    /// A human-readable language name for diagnostics (`less`, `styl`, or the raw extension).
    fn label(path: &Path) -> String {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_else(|| "unknown".to_string())
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
                        "stylesheet {} ({}): no pure-Rust {} compiler exists; passed through as raw CSS (SCSS/Sass compile via grass)",
                        path.display(),
                        StyleLang::label(&path),
                        StyleLang::label(&path),
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

/// Apply esbuild-equivalent CSS value minification to every `styles: [...]` string
/// in a COMPILED Ivy module.
///
/// This is the packagr-side reproduction of ng-packagr's esbuild `minify: true`
/// pass. It parses the compiled module, finds every object-literal `styles:`
/// property whose value is an array of string literals (the
/// `ɵɵdefineComponent`/`ɵɵngDeclareComponent` `styles` arrays for both inline
/// `styles` and resolved `styleUrls`), and rewrites each string element with
/// [`crate::css_optimizer::optimize`] — collapsing whitespace and minifying values
/// (`color: blue` → `#00f`, `0px` → `0`, …) while preserving the
/// `_ngcontent-%COMP%` / `_nghost-%COMP%` scoping placeholders.
///
/// Replacement is span-based over the original bytes (applied right-to-left so
/// earlier spans stay valid), so every other byte of the emitted Ivy module is
/// preserved verbatim. If the module does not parse, or carries no optimizable
/// `styles` string, the input is returned unchanged (identity).
pub fn optimize_compiled_styles(compiled: &str) -> String {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true).with_module(true);
    let parsed = Parser::new(&allocator, compiled, source_type).parse();
    if !parsed.errors.is_empty() {
        return compiled.to_string();
    }

    // (string-literal span, optimized-content) for each styles element to rewrite.
    let mut edits: Vec<(u32, u32, String)> = Vec::new();
    collect_styles_edits(&parsed.program, &mut edits);
    if edits.is_empty() {
        return compiled.to_string();
    }

    // Apply right-to-left so earlier byte offsets stay valid.
    edits.sort_by_key(|(start, _, _)| *start);
    let mut out = compiled.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        out.replace_range(start as usize..end as usize, &replacement);
    }
    out
}

/// Walk a compiled program for `styles:` array properties and record a span-edit
/// per string-literal element (the FULL literal span, so the surrounding quotes are
/// replaced too — the optimized content is re-quoted with the literal's own quote
/// style).
fn collect_styles_edits(program: &Program, edits: &mut Vec<(u32, u32, String)>) {
    for stmt in &program.body {
        walk_statement_for_styles(stmt, edits);
    }
}

fn walk_statement_for_styles(stmt: &Statement, edits: &mut Vec<(u32, u32, String)>) {
    use oxc_ast::ast::Declaration;
    match stmt {
        Statement::ExpressionStatement(e) => walk_expr_for_styles(&e.expression, edits),
        Statement::VariableDeclaration(v) => {
            for d in &v.declarations {
                if let Some(init) = &d.init {
                    walk_expr_for_styles(init, edits);
                }
            }
        }
        Statement::ClassDeclaration(c) => walk_class_for_styles(c, edits),
        Statement::ExportNamedDeclaration(e) => {
            if let Some(Declaration::ClassDeclaration(c)) = &e.declaration {
                walk_class_for_styles(c, edits);
            }
            if let Some(Declaration::VariableDeclaration(v)) = &e.declaration {
                for d in &v.declarations {
                    if let Some(init) = &d.init {
                        walk_expr_for_styles(init, edits);
                    }
                }
            }
        }
        Statement::ExportDefaultDeclaration(_) => {}
        _ => {}
    }
}

/// A class may carry `static ɵcmp = i0.ɵɵdefineComponent({ styles: [...] })` as a
/// member (the `class X { static ɵcmp = … }` shape ng-packagr emits) — walk member
/// initializers too.
fn walk_class_for_styles(class: &Class, edits: &mut Vec<(u32, u32, String)>) {
    for member in &class.body.body {
        if let oxc_ast::ast::ClassElement::PropertyDefinition(p) = member
            && let Some(value) = &p.value
        {
            walk_expr_for_styles(value, edits);
        }
    }
}

fn walk_expr_for_styles(expr: &Expression, edits: &mut Vec<(u32, u32, String)>) {
    match expr {
        Expression::CallExpression(call) => {
            for arg in &call.arguments {
                if let Some(e) = arg.as_expression() {
                    walk_expr_for_styles(e, edits);
                }
            }
        }
        // `BoxComponent.ɵcmp = i0.ɵɵdefineComponent({ styles: [...] })` — the
        // definition lives on the RHS of a static-field assignment statement.
        Expression::AssignmentExpression(assign) => {
            walk_expr_for_styles(&assign.right, edits);
        }
        Expression::ObjectExpression(obj) => {
            for prop in &obj.properties {
                if let ObjectPropertyKind::ObjectProperty(op) = prop {
                    if key_name(&op.key) == Some("styles") {
                        record_styles_array(&op.value, edits);
                    } else {
                        // Recurse into nested object/array values (defensive).
                        walk_expr_for_styles(&op.value, edits);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Record a span-edit for each string-literal element of a `styles:` array value.
fn record_styles_array(value: &Expression, edits: &mut Vec<(u32, u32, String)>) {
    use oxc_span::GetSpan;
    let Expression::ArrayExpression(arr) = value else {
        return;
    };
    for el in &arr.elements {
        let Some(expr) = el.as_expression() else { continue };
        match expr {
            Expression::StringLiteral(lit) => {
                // No trailing newline: esbuild appends one, but Angular's FULL-mode
                // scoping (which treaty applies, and which ng-packagr's `full`
                // compilationMode applies) strips trailing whitespace from the
                // scoped CSS — so the emitted `styles` literal ends at `}` exactly
                // as ng-packagr's full-mode FESM does (`".box[…]{…}"`).
                let optimized = crate::css_optimizer::optimize(&lit.value);
                let span = lit.span;
                edits.push((span.start, span.end, quote_css(&optimized)));
            }
            Expression::TemplateLiteral(tpl)
                if tpl.expressions.is_empty() && tpl.quasis.len() == 1 =>
            {
                if let Some(cooked) = tpl.quasis[0].value.cooked.as_ref() {
                    let optimized = crate::css_optimizer::optimize(cooked);
                    let span = tpl.span();
                    edits.push((span.start, span.end, quote_css(&optimized)));
                }
            }
            _ => {}
        }
    }
}

/// Wrap optimized CSS in a double-quoted JS string literal, escaping the
/// characters that must not appear raw (`"`, `\`, newlines). The optimizer collapses
/// CSS whitespace so embedded newlines are rare, but escape defensively.
fn quote_css(css: &str) -> String {
    let mut out = String::with_capacity(css.len() + 2);
    out.push('"');
    for c in css.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimize_compiled_styles_minifies_define_component_styles() {
        // A realistic AOT-compiled component carrying a SCOPED inline style, with
        // the barred-o `ɵ` member names treaty_ivy emits.
        let compiled = "import * as i0 from \"@angular/core\";\n\
            class BoxComponent {}\n\
            BoxComponent.\u{0275}cmp = i0.\u{0275}\u{0275}defineComponent({\n\
              type: BoxComponent,\n\
              selectors: [[\"acme-box\"]],\n\
              styles: [\".box[_ngcontent-%COMP%] {\\n  color: blue;\\n  margin: 0px;\\n}\"]\n\
            });\n\
            export { BoxComponent };\n";
        let out = optimize_compiled_styles(compiled);
        assert_ne!(out, compiled, "optimizer must rewrite the styles array");
        assert!(out.contains("color:#00f"), "color not minified:\n{out}");
        assert!(out.contains("margin:0"), "zero unit not stripped:\n{out}");
        assert!(out.contains("[_ngcontent-%COMP%]"), "placeholder lost:\n{out}");
        // The Ivy structure is otherwise preserved verbatim.
        assert!(out.contains("\u{0275}\u{0275}defineComponent"), "define lost:\n{out}");
        assert!(out.contains("export { BoxComponent };"), "export lost:\n{out}");
    }

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
