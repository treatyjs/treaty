//! `.treaty` single-file-component compiler (Rust port of the REPL's `treat-to-ivy.ts`).
//!
//! Splits a `.treaty` source into JavaScript / HTML / CSS chunks using the crate's own
//! [`crate::treaty`] lexer + parser (no regex), derives a standalone component, and emits the
//! `ɵɵdefineComponent({...})` definition via the `render3` Ivy engine.
//!
//! Pipeline (mirrors `apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts`):
//! ```text
//! treaty::Lexer  -> tokens
//! treaty::Parser -> Ast { nodes }
//!   JavaScript nodes -> (script, currently unused by render3's template emitter)
//!   Html nodes       -> template
//!   Style nodes      -> styles
//! treaty_ivy::ml_parser + template_transform -> r3_ast
//! render3 compile_component_from_metadata  -> ɵɵdefineComponent
//! ```
//!
//! Component derivation:
//!   * class name: PascalCase of `file_name` (stem only, extension + trailing `.component` stripped)
//!   * selector:   kebab-case of `file_name` (the Treaty convention when the author gives none),
//!                 replacing Angular's `ng-component` no-selector default; sibling components still
//!                 resolve by class name through the selectorless binder
//!   * standalone: `true`
//!   * template:   the joined HTML chunks
//!   * styles:     the CSS chunks (newlines/tabs stripped, as the TS pipeline does)
//!
//! Component references in the template resolve by class name through render3's selectorless
//! binder, and template dependencies are auto-collected — there are no manual `imports`.

use oxc_allocator::Allocator;
use oxc_ast::ast::Expression;
use oxc_parser::Parser as JsParser;
use oxc_span::SourceType;

use treaty_ivy::compile::{CompiledComponent, RealTemplateBuilder};
use treaty_ivy::output::emitter::{emit_expression, emit_expression_with_map, emit_statements};
use treaty_ivy::output_ast::{self as o, ParseSourceSpan};
use treaty_ivy::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use treaty_ivy::util::{R3CompiledExpression, R3Reference};
use treaty_ivy::view::compiler::{
    compile_component_from_metadata, ChangeDetection, ChangeDetectionStrategy, ComponentTemplate,
    DeclarationListEmitMode, Deps, Lifecycle, OrderedMap, R3ComponentDeferMetadata,
    R3ComponentMetadata, R3DirectiveMetadata, R3HostMetadata, R3InputMetadata,
    R3TemplateDependencyKind, R3TemplateDependencyMetadata, StubHostBindingsBuilder,
    ViewEncapsulation,
};

use crate::plugin::{
    extract_server_block_treaty, rewrite_call_sites, BackendEmit, PluginRegistry, ServerFn,
};
use crate::source_map::redact_server_bodies_in_map;
use crate::treaty::ast::AstNode;
use crate::treaty::lexer::Lexer;
use crate::treaty::parser::Parser;
use crate::CompiledAuthoring;

/// A `<style>` chunk: its raw content plus optional preprocessor language (`lang="scss"`).
#[derive(Debug)]
struct StyleChunk {
    content: String,
    lang: Option<String>,
}

/// The source kinds extracted from a `.treaty` file.
#[derive(Debug, Default)]
struct TreatyChunks {
    javascript: Vec<String>,
    html: Vec<String>,
    styles: Vec<StyleChunk>,
    /// Raw bodies of compile-time `Macro` chunks. Captured but NOT executed (macro execution is a
    /// later phase); kept out of the JS/template/CSS output so they never break compilation.
    macros: Vec<String>,
}

/// Lex + parse `source` and bucket its nodes into JavaScript / HTML / CSS chunks.
///
/// Uses the crate's own treaty lexer/parser (no regex). The treaty lexer keeps `{{ … }}`
/// interpolation *inside* the surrounding HTML chunk, so the HTML bucket is a faithful template.
fn split_chunks(source: &str) -> TreatyChunks {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token() {
        tokens.push(token);
    }

    let mut parser = Parser::new(tokens);
    let ast = parser.parse();

    let mut chunks = TreatyChunks::default();
    for node in ast.nodes {
        match node {
            AstNode::JavaScript(code) => chunks.javascript.push(code),
            AstNode::Html(html) => chunks.html.push(html),
            AstNode::Style { content, lang } => chunks.styles.push(StyleChunk { content, lang }),
            // A compile-time macro block: capture its raw body for a later execution phase. It is
            // NOT runtime JS/template/CSS, so it never reaches the component output.
            AstNode::Macro { content, .. } => chunks.macros.push(content),
            // A first-class control-flow region (R1) — `@if`/`@for`/`@switch`/`@defer` + chained
            // clauses, even when NOT wrapped in a host element — is TEMPLATE markup: route its
            // verbatim source into the HTML bucket so it joins the component template, where
            // treaty_ivy's ml_parser lowers the Angular block syntax to control-flow Ivy instructions
            // (`ɵɵconditional`/`ɵɵrepeater`/…). Previously this node was dropped, silently losing a
            // top-level control-flow block.
            AstNode::ControlFlow { verbatim, .. } => chunks.html.push(verbatim),
            // A `server { … }` block is server-only: it is lifted by the server-fn extraction before
            // the client is lexed, so it should not appear here; if it does (defensive), it is NEVER
            // routed into a client chunk so server code can never reach the client.
            AstNode::ServerBlock { .. } => {}
            // A bare top-level interpolation marker carries no standalone body in the common case
            // (it lives inside an HTML chunk); ignore it here.
            AstNode::TemplateExpression(_) | AstNode::EOF => {}
        }
    }
    chunks
}

/// Build a byte-offset-identical "detection view" of a `.treaty` source in which every NON-JavaScript
/// region (HTML, `<style>`, the top-of-file macro fence, control-flow markers) is blanked to
/// length-preserving whitespace, leaving the JavaScript regions intact in place.
///
/// A `.treaty` SFC interleaves TypeScript with markup that is not valid TypeScript, so a whole-file TS
/// parse — which the parse-based server-fn MARKER lift (`'use server'` / `$$` / `'use websocket'`)
/// needs — fails and the marker in the JS region is never seen. Masking the non-JS regions to
/// whitespace (every non-newline byte → a space, newlines preserved so line/column spans stay aligned)
/// yields a view that parses as TS and whose every byte sits at the same index as in the real source,
/// so the marker spans map 1:1 back. The lexer's [`Token::start`]/`end` byte offsets drive the mask, so
/// it is exact (never a regex). Bytes the lexer does not cover (whitespace/gaps between tokens) stay as
/// they are — they are already insignificant whitespace.
fn mask_non_js_regions(source: &str) -> String {
    use crate::treaty::token::TokenKind;
    let bytes = source.as_bytes();
    // Start from a copy; blank only the spans of non-JavaScript tokens.
    let mut out: Vec<u8> = bytes.to_vec();

    let mut lexer = Lexer::new(source);
    while let Some(token) = lexer.next_token() {
        if matches!(token.kind, TokenKind::JavaScript(_)) {
            continue;
        }
        // Blank this non-JS region to whitespace, preserving byte length AND newlines (so span line
        // numbers in the detection parse line up with the real source).
        let start = token.start.min(out.len());
        let end = token.end.min(out.len());
        for b in &mut out[start..end] {
            if *b != b'\n' && *b != b'\r' {
                *b = b' ';
            }
        }
    }

    // `out` is a byte-for-byte length-preserving transform of valid UTF-8 (only non-newline bytes in
    // masked regions were set to ASCII space, never splitting a multibyte char because a masked region
    // is replaced wholesale on its own token boundaries), so it is valid UTF-8.
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// PascalCase the *stem* of a file name (path separators, extension, and a trailing `.component`
/// segment dropped).
///
/// `"hello-world.treaty"` -> `"HelloWorld"`, `"my_widget.treaty"` -> `"MyWidget"`,
/// `"log-viewer.component.ts"` -> `"LogViewer"`.
fn to_pascal_case(file_name: &str) -> String {
    // Drop directory components, the extension, and a trailing `.component` segment.
    let stem = component_stem(file_name);

    let mut out = String::new();
    let mut new_word = true;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
                new_word = false;
            } else {
                out.push(ch);
            }
        } else {
            // Any separator (space, '-', '_', etc.) starts a new word.
            new_word = true;
        }
    }

    if out.is_empty() {
        "TreatyComponent".to_string()
    } else {
        out
    }
}

/// The bare file-name stem used for both name and selector derivation: directory components and the
/// authoring extension are dropped, and a trailing `.component` segment (the Angular `foo.component`
/// convention) is stripped so `log-viewer.component.ts` and `gauge.treaty` both reduce to their
/// logical component name (`log-viewer`, `gauge`). The remaining stem keeps its author-written word
/// separators (`-`/`_`/space) so it can be PascalCased or kebab-cased downstream.
fn component_stem(file_name: &str) -> &str {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    // Drop the authoring extension (the final `.ext`); a name with no dot keeps its whole self.
    let no_ext = base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base);
    // Strip a trailing `.component` segment (`foo.component` -> `foo`), the Angular file convention.
    no_ext
        .strip_suffix(".component")
        .unwrap_or(no_ext)
}

/// Derive a kebab-case element selector from a file name, the Treaty convention for a component that
/// declares no explicit selector. The authoring extension and a trailing `.component` segment are
/// dropped, then the stem is lowercased with every run of non-alphanumeric characters collapsed to a
/// single `-` and a digit-letter / letter-digit / case boundary treated as a word break:
///
/// `"counter.tsx"` -> `"counter"`, `"greeting-card.tjsx"` -> `"greeting-card"`,
/// `"log-viewer.component.ts"` -> `"log-viewer"`, `"MyWidget.treaty"` -> `"my-widget"`. Digits stay
/// attached to their adjacent run (`"chart2d"` -> `"chart2d"`); only separators and case transitions
/// introduce a word break.
///
/// Used as the selector ONLY when the author gives none; it replaces Angular's `ng-component`
/// no-selector default so a bootstrapped Treaty component has a real host tag instead of
/// `<ng-component>`.
///
/// Public within the crate so the base-Angular `.ts` front-end ([`crate::angular_source`]) can pass
/// the same filename-derived selector to a SELECTORLESS `@Component`, keeping the JSX / `.treaty` /
/// `.ts` authoring formats aligned on one convention.
pub fn to_kebab_case(file_name: &str) -> String {
    let stem = component_stem(file_name);

    let mut out = String::new();
    let mut prev_was_alnum = false;
    let mut prev_lower_or_digit = false;
    let chars: Vec<char> = stem.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_alphanumeric() {
            // Insert a boundary `-` at a camelCase transition (`MyWidget` -> `my-widget`,
            // `HTTPClient` -> `http-client`) when the previous char was alphanumeric. Digits attach
            // to their neighboring run, so `chart2d` stays one word.
            let is_upper = ch.is_ascii_uppercase();
            let next_is_lower = chars.get(i + 1).is_some_and(|c| c.is_ascii_lowercase());
            let case_boundary = prev_was_alnum
                && ((is_upper && prev_lower_or_digit)
                    || (is_upper && next_is_lower && !out.is_empty()));
            if case_boundary && !out.ends_with('-') {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
            prev_was_alnum = true;
            prev_lower_or_digit = !is_upper;
        } else {
            // Any separator collapses to a single `-` (no leading/trailing/duplicate dashes).
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            prev_was_alnum = false;
            prev_lower_or_digit = false;
        }
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "treaty-component".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The SELECTORLESS default selector for `file_name`: a comma-separated CSS selector list covering
/// the component name's kebab-case, camelCase and PascalCase forms, so a parent can reference the
/// child with ANY of `<greeting-card>`, `<greetingCard>` or `<GreetingCard>` (Treaty's selectorless
/// convention — "use the class name as the selector in kebab/camel/pascal"). The R3 selector parser
/// splits the comma list into one selector entry per form (`parse_css_selectors`), so the emitted
/// `selectors: [["greeting-card"],["greetingCard"],["GreetingCard"]]` matches every spelling.
/// Single-word names collapse (kebab == camel) to two entries (`greeter, Greeter`).
pub fn to_multi_selector(file_name: &str) -> String {
    let kebab = to_kebab_case(file_name);
    // PascalCase by upper-casing each kebab segment's first letter; camelCase lower-cases Pascal's.
    let mut pascal = String::new();
    for seg in kebab.split('-').filter(|s| !s.is_empty()) {
        let mut chars = seg.chars();
        if let Some(first) = chars.next() {
            pascal.extend(first.to_uppercase());
            pascal.push_str(chars.as_str());
        }
    }
    let camel = {
        let mut chars = pascal.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().chain(chars).collect::<String>(),
            None => String::new(),
        }
    };
    // Deduplicate while preserving order (kebab, camel, pascal); a single-word name has kebab == camel.
    let mut forms: Vec<String> = Vec::new();
    for form in [kebab, camel, pascal] {
        if !form.is_empty() && !forms.contains(&form) {
            forms.push(form);
        }
    }
    forms.join(", ")
}

/// Recognizes a signal initializer call: `input()`, `input.required()`, `model()`,
/// `model.required()`, `output()`. Returns the base callee identifier (`input`/`model`/`output`)
/// and whether `.required` was used. Mirrors `treaty_ivy::source_compile::signal_call`.
fn signal_call<'a>(expr: &'a Expression<'a>) -> Option<(&'a str, bool)> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    match &call.callee {
        // `input(...)`, `output(...)`, `model(...)`
        Expression::Identifier(id) => Some((id.name.as_str(), false)),
        // `input.required(...)`, `model.required(...)`
        Expression::StaticMemberExpression(member) => {
            if let Expression::Identifier(base) = &member.object {
                let required = member.property.name.as_str() == "required";
                Some((base.name.as_str(), required))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Read the `alias` string from a signal-input call's options object — `input(<default>, { alias:
/// "public" })` → `Some("public")`. Returns `None` when the call has no second-argument options
/// object, or it carries no string-valued `alias` key. Used so a renamed input (the JSX param-form
/// `{ label: caption }`, lowered to `input(undefined, { alias: "label" })`) surfaces its PUBLIC name
/// as the input's `binding_property_name`.
fn input_alias<'a>(expr: &'a Expression<'a>) -> Option<String> {
    use oxc_ast::ast::{Argument, Expression as E, ObjectPropertyKind, PropertyKey};
    let E::CallExpression(call) = expr else {
        return None;
    };
    // The options object is the SECOND argument (`input(<default>, { … })`).
    let Argument::ObjectExpression(obj) = call.arguments.get(1)? else {
        return None;
    };
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            continue;
        };
        let key = match &p.key {
            PropertyKey::StaticIdentifier(id) => id.name.as_str(),
            PropertyKey::StringLiteral(s) => s.value.as_str(),
            _ => continue,
        };
        if key == "alias" {
            if let E::StringLiteral(s) = &p.value {
                return Some(s.value.to_string());
            }
        }
    }
    None
}

/// One destructured property lifted out of a `const { … } = input<Props>()` declaration:
/// the LOCAL binding name (the runtime signal name), the PUBLIC input name (`alias`, which differs
/// from `local` only for a `{ key: local }` rename), the optional default-value source text
/// (`{ local = <default> }`), and whether the destructured initializer was `input.required()` /
/// `model()`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DestructuredInput {
    /// The local binding identifier — becomes a `const <local> = input(...)` runtime signal.
    pub local: String,
    /// The PUBLIC input name. Equals `local` unless the author wrote a `{ alias: local }` rename, in
    /// which case it is the object key — the name a parent binds to (the input's
    /// `binding_property_name`).
    pub alias: String,
    /// Verbatim default-value source text (`{ local = <default> }`), already TS-erased; `None` when
    /// the property carries no default.
    pub default: Option<String>,
    /// `input.required()` / `model.required()` — the lifted property is a REQUIRED input.
    pub required: bool,
    /// The initializer was `model()` (not `input()`): the lifted property is a two-way signal and
    /// also yields a paired `<local>Change` output.
    pub is_model: bool,
}

/// Expand every top-level `const { … } = input<Props>()` / `model<Props>()` object-destructure in
/// `javascript` into one individual `const <local> = input(<default?>)` declaration per property.
///
/// This is the AUTHORING ergonomics feature "object destructuring → inputs": a component may declare
/// its inputs in one destructuring statement —
/// ```ignore
/// const { name, age = 0, label: caption } = input<Props>();
/// ```
/// — and each destructured property becomes an INDIVIDUAL component input. The expansion lowers that
/// to the per-property `input()` form the rest of the pipeline (signal lowering, binding collection,
/// `defineComponent` emit) already understands:
/// ```ignore
/// const name = input();
/// const age = input(0);
/// const caption = input();
/// ```
/// The PUBLIC name of a renamed property (`{ label: caption }` → public `label`, local `caption`) is
/// NOT carried in the emitted `input()` call (the runtime signal is just the local); it is recorded
/// separately by [`collect_destructured_inputs`] and threaded into the input's
/// `binding_property_name` so a parent still binds by the public name.
///
/// Only a SINGLE-declarator `const { … } = <input-call>` whose initializer is `input()` /
/// `input.required()` / `model()` is rewritten — any other destructure (e.g. `const { x } = obj`) is
/// left untouched so unrelated destructuring is never disturbed. A `...rest` element is dropped (it
/// cannot be enumerated into discrete inputs) and recorded as a non-fatal note by the collector.
/// Returns the source unchanged when it contains no such declaration, so the common case is a
/// byte-for-byte passthrough.
pub(crate) fn expand_input_destructures(javascript: &str) -> String {
    if javascript.trim().is_empty() {
        return javascript.to_string();
    }
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    // (span_to_replace, replacement_text), highest span first so earlier offsets stay valid.
    let mut replacements: Vec<((usize, usize), String)> = Vec::new();
    for stmt in &ret.program.body {
        let oxc_ast::ast::Statement::VariableDeclaration(decl) = stmt else {
            continue;
        };
        let Some((props, _required, is_model)) = input_destructure_decl(decl, javascript) else {
            continue;
        };
        let kind = if is_model { "model" } else { "input" };
        let lines: Vec<String> = props
            .iter()
            .map(|p| match &p.default {
                Some(d) => format!("const {} = {}({});", p.local, kind, d),
                None => format!("const {} = {}();", p.local, kind),
            })
            .collect();
        replacements.push((
            (decl.span.start as usize, decl.span.end as usize),
            lines.join("\n"),
        ));
    }
    if replacements.is_empty() {
        return javascript.to_string();
    }
    replacements.sort_by(|a, b| b.0 .0.cmp(&a.0 .0));
    let mut out = javascript.to_string();
    for ((start, end), text) in replacements {
        out.replace_range(start..end, &text);
    }
    out
}

/// Collect the `{ … } = input<Props>()` destructured inputs from the ORIGINAL (un-expanded) body JS,
/// each with its public alias / default / required / model classification. Used by [`extract_io`] to
/// register one component input per destructured property with the correct `binding_property_name`.
pub(crate) fn collect_destructured_inputs(javascript: &str) -> Vec<DestructuredInput> {
    if javascript.trim().is_empty() {
        return Vec::new();
    }
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    let mut out: Vec<DestructuredInput> = Vec::new();
    for stmt in &ret.program.body {
        let oxc_ast::ast::Statement::VariableDeclaration(decl) = stmt else {
            continue;
        };
        if let Some((props, _required, _is_model)) = input_destructure_decl(decl, javascript) {
            out.extend(props);
        }
    }
    out
}

/// If `decl` is a single-declarator `const { … } = input()/input.required()/model()` object
/// destructure, return its destructured properties plus whether the initializer was `.required()` and
/// whether it was `model()`. Returns `None` for any other declaration shape so unrelated destructuring
/// (`const { x } = obj`) is never matched.
fn input_destructure_decl<'a>(
    decl: &oxc_ast::ast::VariableDeclaration<'a>,
    source: &str,
) -> Option<(Vec<DestructuredInput>, bool, bool)> {
    use oxc_ast::ast::BindingPattern;
    // Only a single-declarator declaration carries one initializer to classify.
    if decl.declarations.len() != 1 {
        return None;
    }
    let declarator = &decl.declarations[0];
    // The binding target must be an object pattern (`{ … }`).
    let BindingPattern::ObjectPattern(obj) = &declarator.id else {
        return None;
    };
    // The initializer must be an `input()` / `input.required()` / `model()` signal call.
    let init = declarator.init.as_ref()?;
    let (base, required) = signal_call(init)?;
    if base != "input" && base != "model" {
        return None;
    }
    let is_model = base == "model";

    let mut props: Vec<DestructuredInput> = Vec::new();
    for prop in &obj.properties {
        // The bound local (`a` for `{ a }`; `local` for `{ key: local }`; the assignment target for
        // `{ a = 5 }`). A nested destructure has no single local — skip it.
        let Some(local) = binding_pattern_local_name(&prop.value) else {
            continue;
        };
        // The PUBLIC name is the object key for a shorthand/rename; default it to the local when the
        // key is a non-identifier (computed) key.
        let alias = property_key_name(&prop.key).unwrap_or_else(|| local.clone());
        let default = binding_pattern_default_text(&prop.value, source)
            .map(|d| crate::jsx::ts_erase::erase_via_reparse(&d).unwrap_or(d));
        props.push(DestructuredInput {
            local,
            alias,
            default,
            required,
            is_model,
        });
    }
    if props.is_empty() {
        return None;
    }
    Some((props, required, is_model))
}

/// The single local identifier bound by a binding pattern: `a` for `a`, the assignment target for
/// `a = <default>`. `None` for a nested array/object destructure (no single local name).
fn binding_pattern_local_name(pat: &oxc_ast::ast::BindingPattern) -> Option<String> {
    use oxc_ast::ast::BindingPattern;
    match pat {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        BindingPattern::AssignmentPattern(assign) => binding_pattern_local_name(&assign.left),
        _ => None,
    }
}

/// The verbatim default-value source text of a defaulted binding pattern (`5` for `a = 5`), sliced
/// from `source` (the body the pattern was parsed from). `None` when it carries no default.
fn binding_pattern_default_text(
    pat: &oxc_ast::ast::BindingPattern,
    source: &str,
) -> Option<String> {
    use oxc_ast::ast::BindingPattern;
    let BindingPattern::AssignmentPattern(assign) = pat else {
        return None;
    };
    let span = oxc_span::GetSpan::span(&assign.right);
    source
        .get(span.start as usize..span.end as usize)
        .map(|t| t.trim().to_string())
}

/// The identifier text of a property key (`a` for `{ a: x }` / `{ a }`), or `None` for a computed key.
fn property_key_name(key: &oxc_ast::ast::PropertyKey) -> Option<String> {
    use oxc_ast::ast::PropertyKey;
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
        _ => None,
    }
}

/// Parse the component-body JS chunk and extract Treaty signal inputs/outputs.
///
/// In the Treaty SFC model the top-level JS *is* the component body, so top-level
/// `const <name> = input()/input.required()/model()/output()` declarations become the
/// component's signal inputs/outputs. Mirrors `treaty_ivy::source_compile` signal extraction:
///   * `input()`            → signal input
///   * `input.required()`   → required signal input
///   * `model()`            → signal input + paired `<name>Change` output
///   * `output()`           → output
///
/// Parse failures are non-fatal: the JS chunk is the user's free-form body and may use syntax
/// the template path does not care about, so an unparseable chunk simply yields no I/O.
fn extract_io(
    javascript: &str,
    inputs: &mut OrderedMap<String, R3InputMetadata>,
    outputs: &mut OrderedMap<String, String>,
) {
    if javascript.trim().is_empty() {
        return;
    }

    // First lift every `const { … } = input<Props>()` object-destructure: each destructured property
    // becomes an INDIVIDUAL component input. A renamed property (`{ key: local }`) keeps `local` as
    // the runtime signal / class-property name and `key` as the PUBLIC `binding_property_name`, so a
    // parent still binds by the public name. (Defaults carry no input metadata — they affect only the
    // runtime `input(<default>)` call the body expansion emits.) Runs over the ORIGINAL body so the
    // rename's public key is still visible (the body expansion lowers `{ key: local }` to a bare
    // `const local = input()`, dropping the key).
    for d in collect_destructured_inputs(javascript) {
        inputs.insert(
            d.local.clone(),
            R3InputMetadata {
                class_property_name: d.local.clone(),
                binding_property_name: d.alias.clone(),
                required: d.required,
                is_signal: true,
                transform_function: None,
            },
        );
        if d.is_model {
            let change = format!("{}Change", d.local);
            outputs.insert(change.clone(), change);
        }
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    for stmt in &ret.program.body {
        let oxc_ast::ast::Statement::VariableDeclaration(decl) = stmt else {
            continue;
        };
        for declarator in &decl.declarations {
            let Some(name) = declarator.id.get_identifier_name() else {
                continue;
            };
            let name = name.to_string();
            let Some(init) = &declarator.init else {
                continue;
            };
            let Some((base, required)) = signal_call(init) else {
                continue;
            };
            match base {
                "input" | "model" => {
                    // A renamed prop reaches the body as `input(<default>, { alias: "<public>" })`
                    // (e.g. the JSX param-form rename `{ label: caption }`), so the PUBLIC binding name
                    // is the alias, falling back to the local declaration name when none is present.
                    let binding = input_alias(init).unwrap_or_else(|| name.clone());
                    inputs.insert(
                        name.clone(),
                        R3InputMetadata {
                            class_property_name: name.clone(),
                            binding_property_name: binding,
                            required,
                            is_signal: true,
                            transform_function: None,
                        },
                    );
                    if base == "model" {
                        let change = format!("{name}Change");
                        outputs.insert(change.clone(), change);
                    }
                }
                "output" => {
                    outputs.insert(name.clone(), name.clone());
                }
                _ => {}
            }
        }
    }
}

/// Collect the JS chunk's imported identifier names — the auto-import candidate set. Mirrors
/// `extractImportStrings` in `treat-to-ivy.ts`, but over the AST: every default, namespace and
/// named binding introduced by an `import` declaration. A parse failure yields no candidates.
fn collect_imported_names(javascript: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if javascript.trim().is_empty() {
        return names;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    for stmt in &ret.program.body {
        let oxc_ast::ast::Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        for spec in specifiers {
            match spec {
                oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
                oxc_ast::ast::ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
                oxc_ast::ast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
            }
        }
    }
    names
}

/// Pieces extracted from the component-body JS chunk for module assembly.
///
/// Mirrors `createWrapper`/`extractImportStrings`/`removeImportsFromCode` in
/// `apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts`:
///   * `imports` — the import declarations, sliced verbatim from the source.
///   * `hoisted` — top-level declarations whose name is referenced by the component's
///     `dependencies: […]` (a locally-declared directive). These MUST live at MODULE scope — beside
///     the component class — because `<Comp>.ɵcmp = ɵɵdefineComponent({ dependencies: [Foo] })` reads
///     `Foo` at module scope; left inside the synthesized `function <Comp>() { … }` wrapper they would
///     be wrapper-locals and `dependencies: [Foo]` would dangle (`Foo is not defined` at boot).
///   * `body`    — the JS chunk with its import declarations AND hoisted declarations removed.
///   * `bindings`— the remaining top-level `const`/`function` declaration names, for the returned
///     object (component state). Hoisted directive declarations are NOT component state, so they are
///     excluded from `bindings`.
#[derive(Default)]
struct WrapperParts {
    imports: Vec<String>,
    hoisted: Vec<String>,
    body: String,
    bindings: Vec<String>,
}

/// Parse the component-body JS chunk and collect: import statements (verbatim, module scope), any
/// top-level declaration whose name is in `module_scope_names` (verbatim, hoisted to module scope),
/// the body with both removed, and the remaining top-level `const`/`function` declaration names
/// (component state bindings).
///
/// `module_scope_names` is the set of `dependencies: […]` identifiers the component references — a
/// locally-declared directive (e.g. `counter.tsx`'s `highlight`) must be emitted beside the component
/// class, not buried in its wrapper, so the dependency reference resolves.
fn extract_wrapper_parts(javascript: &str, module_scope_names: &[String]) -> WrapperParts {
    let mut parts = WrapperParts::default();
    if javascript.trim().is_empty() {
        return parts;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    let is_module_scope = |name: &str| module_scope_names.iter().any(|n| n == name);

    // Byte ranges to splice out of the body: import declarations (always) and hoisted module-scope
    // declarations (a locally-declared directive referenced by `dependencies`).
    let mut removed_ranges: Vec<(usize, usize)> = Vec::new();

    for stmt in &ret.program.body {
        match stmt {
            oxc_ast::ast::Statement::ImportDeclaration(import) => {
                let start = import.span.start as usize;
                let end = import.span.end as usize;
                parts.imports.push(javascript[start..end].to_string());
                removed_ranges.push((start, end));
            }
            oxc_ast::ast::Statement::VariableDeclaration(decl) => {
                // A single-declarator `const Foo = …` whose name is a module-scope directive is
                // hoisted whole; otherwise its declarator names are component-state bindings.
                let names: Vec<String> = decl
                    .declarations
                    .iter()
                    .filter_map(|d| d.id.get_identifier_name().map(|n| n.to_string()))
                    .collect();
                if names.len() == 1 && is_module_scope(&names[0]) {
                    let start = decl.span.start as usize;
                    let end = decl.span.end as usize;
                    parts.hoisted.push(javascript[start..end].to_string());
                    parts.hoisted.push("\n".to_string());
                    removed_ranges.push((start, end));
                } else {
                    parts.bindings.extend(names);
                }
            }
            oxc_ast::ast::Statement::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    let name = id.name.to_string();
                    if is_module_scope(&name) {
                        let start = func.span.start as usize;
                        let end = func.span.end as usize;
                        parts.hoisted.push(javascript[start..end].to_string());
                        parts.hoisted.push("\n".to_string());
                        removed_ranges.push((start, end));
                    } else {
                        parts.bindings.push(name);
                    }
                }
            }
            oxc_ast::ast::Statement::ClassDeclaration(class) => {
                // A locally-declared directive may be a `class`; hoist it to module scope when it is
                // a referenced dependency. (A non-dependency class stays in the wrapper body verbatim
                // and is not a component-state binding.)
                if let Some(id) = &class.id {
                    let name = id.name.to_string();
                    if is_module_scope(&name) {
                        let start = class.span.start as usize;
                        let end = class.span.end as usize;
                        parts.hoisted.push(javascript[start..end].to_string());
                        parts.hoisted.push("\n".to_string());
                        removed_ranges.push((start, end));
                    }
                }
            }
            _ => {}
        }
    }

    // Body = source with the removed (import + hoisted) byte ranges spliced out, in source order.
    removed_ranges.sort_by_key(|(start, _)| *start);
    if removed_ranges.is_empty() {
        parts.body = javascript.to_string();
    } else {
        let mut body = String::with_capacity(javascript.len());
        let mut cursor = 0usize;
        for (start, end) in &removed_ranges {
            if *start >= cursor {
                body.push_str(&javascript[cursor..*start]);
                cursor = *end;
            }
        }
        body.push_str(&javascript[cursor..]);
        parts.body = body;
    }

    parts
}

/// Assemble the full runnable ES module string, mirroring the TS `createWrapper`.
///
/// `render3`'s `emit_expression` prefixes the defineComponent expression with its own
/// `import * as i0 from "@angular/core";` line; the module emits that import once at the top, so
/// any such leading prefix is stripped from the `.ɵcmp` value here.
fn build_module(
    class_name: &str,
    javascript: &str,
    cmp_expression: &str,
    module_scope_names: &[String],
    pool_statements: &str,
) -> String {
    const I0_IMPORT: &str = "import * as i0 from \"@angular/core\";";
    let cmp_expression = cmp_expression
        .strip_prefix(I0_IMPORT)
        .map(str::trim_start)
        .unwrap_or(cmp_expression);
    // The pool statements are emitted as their own module (with a leading `import * as i0` the
    // top-level import already covers); strip that duplicate import line and reuse the single i0.
    let pool_statements = pool_statements
        .strip_prefix(I0_IMPORT)
        .map(str::trim_start)
        .unwrap_or(pool_statements);

    let parts = extract_wrapper_parts(javascript, module_scope_names);

    let mut module = String::new();
    module.push_str("import * as i0 from \"@angular/core\";\n");
    for import in &parts.imports {
        module.push_str(import);
        module.push('\n');
    }
    // Module-scope declarations (a locally-declared directive referenced by `dependencies`) are
    // emitted here — beside the component class — so the `dependencies: [Foo]` reference resolves.
    for hoisted in &parts.hoisted {
        module.push_str(hoisted);
    }
    // Hoisted nested-view template functions + shared const-pool literals the component's template
    // references. Module-scope siblings, declared before the component (the template reads them at
    // render time), mirroring how the base `@Component` path emits the ConstantPool statements.
    if !pool_statements.trim().is_empty() {
        module.push_str(pool_statements.trim_end());
        module.push('\n');
    }
    module.push_str(&format!("function {class_name}() {{\n"));
    module.push_str(parts.body.trim());
    module.push_str(&format!(
        "\nreturn {{ {} }};\n}}\n",
        parts.bindings.join(", ")
    ));
    module.push_str(&format!(
        "{class_name}.\u{0275}fac = function {class_name}_Factory(t) {{ return (t || {class_name})(); }};\n"
    ));
    module.push_str(&format!("{class_name}.\u{0275}cmp = {cmp_expression};\n"));
    module.push_str(&format!("export default {class_name};\n"));
    module
}

fn class_ref(class_name: &str) -> R3Reference {
    R3Reference {
        value: o::variable(class_name, None),
        ty: o::variable(class_name, None),
    }
}

/// Compile a `.treaty` SFC source into a full runnable ES module.
///
/// `file_name` derives the component class name (PascalCase of the stem). The component is
/// standalone, selectorless (class-name based), with the HTML chunk as its template and the CSS
/// chunk as its styles.
///
/// The emitted module mirrors the TS `createWrapper` (Treaty's runtime is a *function* component):
/// ```text
/// import * as i0 from "@angular/core";
/// <verbatim import statements from the JS chunk>
/// function <Comp>() { <JS chunk body, imports stripped> return { <const/function names> }; }
/// <Comp>.ɵfac = function <Comp>_Factory(t) { return (t || <Comp>)(); };
/// <Comp>.ɵcmp = ɵɵdefineComponent({ ... });
/// export default <Comp>;
/// ```
pub fn compile_treaty_file(source: &str, file_name: &str) -> CompiledComponent {
    compile_treaty_file_inner(source, file_name, None).0
}

/// Like [`compile_treaty_file`], but ALSO emits an additive Source Map v3 JSON.
///
/// `source_name` / `source_content` describe the ORIGINAL authoring source embedded in the map's
/// `sources[0]` / `sourcesContent[0]`. `source_content` is the verbatim `.treaty` file text (the
/// caller passes the original, pre-server-strip source so the map carries the author's file); the
/// server-aware wrapper then redacts any lifted server-fn body out of `sourcesContent` for client
/// privacy. The `code` is byte-identical to [`compile_treaty_file`]; the map is additive.
pub fn compile_treaty_file_with_map(
    source: &str,
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (CompiledComponent, Option<String>) {
    compile_treaty_file_inner(source, file_name, Some((source_name, source_content)))
}

/// Shared implementation behind [`compile_treaty_file`] and [`compile_treaty_file_with_map`].
///
/// `source` is the (server-stripped) client `.treaty` source that is actually compiled; the
/// optional `source_map` carries the original authoring `(source_name, source_content)` to embed
/// in the emitted map. When `source_map` is `None`, the plain map-free path is used.
fn compile_treaty_file_inner(
    source: &str,
    file_name: &str,
    source_map: Option<(&str, &str)>,
) -> (CompiledComponent, Option<String>) {
    // A `.treaty` SFC may carry a top-level `server { … }` block. Even on this server-UNAWARE
    // entry the block must be lifted out of the client source before lowering: left in place it is
    // not valid JS in the synthesized component function (`server { async function … }` makes the
    // body JS chunk fail to parse, so `extract_wrapper_parts` extracts nothing and the body's
    // `import` declarations leak verbatim INTO the component function — illegal, and esbuild rejects
    // it with `Unexpected "{"`). Stripping the block here keeps every `.treaty` entry point emitting
    // valid client JS; the server MODULE itself is still emitted only by the server-aware
    // `compile_treaty_authoring` path. The marker forms (`'use server'` / `$$` / `'use websocket'`) are
    // lifted too via the masked detection view (see [`mask_non_js_regions`]), so a marker server fn in
    // a `.treaty` JS chunk is stripped from the client even on this server-UNAWARE entry.
    let client_source = extract_server_block_treaty(source, &mask_non_js_regions(source)).client_source;
    let chunks = split_chunks(&client_source);
    let class_name = to_pascal_case(file_name);

    let template_html = chunks.html.join("");
    let mut javascript = chunks.javascript.join("");

    let mut macro_errors: Vec<String> = Vec::new();

    // Execute any top-of-file macro block (server-side render-time code, like Astro frontmatter /
    // RSC) and inject the produced data into the component. The macro is TypeScript; it is run
    // through `treaty_runtime::run_macro`, which transpiles it to JS and evaluates it on the Nova
    // engine. This is the STATIC prerender path: an empty (`null`) input is passed at compile time.
    //
    // The macro's value is injected as a `const $macro = <json>;` declaration prepended to the
    // component-body JS. Because it is a top-level `const`, it is (a) visible to the rest of the
    // body and (b) collected into the component's returned bindings object by
    // `extract_wrapper_parts`, so the template can bind it directly (e.g. `{{ $macro.title }}`).
    // The macro SOURCE itself is never emitted — only its computed data is.
    if let Some(literal) = run_and_encode_macros(&chunks.macros, &mut macro_errors) {
        javascript = if javascript.trim().is_empty() {
            literal
        } else {
            format!("{literal}\n{javascript}")
        };
    }

    let mut style_errors: Vec<String> = Vec::new();

    // Build the component styles. A `<style lang="scss">` / `lang="sass"` chunk is compiled to CSS
    // with the pure-Rust `grass` sass implementation; plain CSS (no lang) passes through unchanged.
    // A sass compile error is recorded as a component error (never panics) and that chunk is
    // dropped. Match the TS pipeline: strip newlines/tabs from the resulting CSS.
    let mut styles: Vec<String> = Vec::new();
    for chunk in &chunks.styles {
        let css = match chunk.lang.as_deref() {
            Some("scss") | Some("sass") => {
                // Compressed output matches the rest of the pipeline (no superfluous whitespace),
                // so `.x { color: $c; }` emits `.x{color:red}`.
                let options =
                    grass::Options::default().style(grass::OutputStyle::Compressed);
                match grass::from_string(chunk.content.clone(), &options) {
                    Ok(css) => css,
                    Err(e) => {
                        style_errors.push(format!("sass: {e}"));
                        continue;
                    }
                }
            }
            _ => chunk.content.clone(),
        };
        let css = css.replace(['\n', '\r', '\t'], "");
        if !css.is_empty() {
            styles.push(css);
        }
    }

    // The resolved CSS chunks join into a single `styles` string for the shared render3 backend.
    // Current `.treaty` sources carry at most one style chunk, so this round-trips byte-identically.
    let styles = styles.join("");

    let (mut compiled, map) = match source_map {
        Some((source_name, source_content)) => compile_from_parts_with_directives_and_map(
            &class_name,
            &javascript,
            &template_html,
            &styles,
            file_name,
            &[],
            source_name,
            source_content,
        ),
        None => (
            compile_from_parts(&class_name, &javascript, &template_html, &styles, file_name),
            None,
        ),
    };
    // Surface macro and sass diagnostics ahead of the template diagnostics from the backend.
    if !macro_errors.is_empty() || !style_errors.is_empty() {
        let mut errors = macro_errors;
        errors.extend(style_errors);
        errors.extend(compiled.errors);
        compiled.errors = errors;
    }
    (compiled, map)
}

/// Run the captured macro block(s) and encode their combined output as a single injectable JS
/// `const` declaration, or `None` when there are no macros.
///
/// Each macro is server-side render-time TypeScript (the top-of-file fenced block). It is executed
/// via [`treaty_runtime::run_macro`] with an empty compile-time input ([`serde_json::Value::Null`])
/// — the static-prerender path. The produced JSON value is injected as `const $macro = <json>;`
/// (or `$macro0` / `$macro1` / … when a file carries more than one macro block) so the component
/// body and template can reference the data. A macro that fails to transpile or throws records its
/// message in `errors` and contributes no binding; the rest of the component still compiles.
///
/// Returns the declaration text to prepend to the component-body JS, or `None` when no macro
/// produced an injectable value.
fn run_and_encode_macros(macros: &[String], errors: &mut Vec<String>) -> Option<String> {
    if macros.is_empty() {
        return None;
    }

    // The static-prerender input. A `.treaty` macro reads request data via `input`; at build time
    // there is no request, so an empty input is supplied. (The dynamic-prerender path re-runs the
    // same macro per request with real input — handled by the runtime layer, not the compiler.)
    let input = serde_json::Value::Null;

    let mut decls: Vec<String> = Vec::new();
    for (idx, macro_src) in macros.iter().enumerate() {
        // A single macro binds plain `$macro`; multiple blocks are disambiguated by index.
        let name = if macros.len() == 1 {
            "$macro".to_string()
        } else {
            format!("$macro{idx}")
        };
        match treaty_runtime::run_macro(macro_src, &input) {
            Ok(output) => {
                // `serde_json::to_string` of any JSON value is a valid JS expression literal, so
                // the splice is injection-safe.
                let literal = serde_json::to_string(output.value()).unwrap_or_else(|_| "null".to_string());
                decls.push(format!("const {name} = {literal};"));
            }
            Err(e) => errors.push(format!("macro: {e}")),
        }
    }

    if decls.is_empty() {
        None
    } else {
        Some(decls.join("\n"))
    }
}

/// Compile a component from already-split parts into the `ɵɵdefineComponent` ES module.
///
/// This is the render3-backend half of [`compile_treaty_file`], factored out so any authoring
/// front-end (the `.treaty` lexer, a JSX transpiler, …) can lower its source to a component class
/// name, a JavaScript body, an Angular template HTML string, and resolved CSS, then reuse the same
/// Ivy codegen path. `styles` is the final CSS (any preprocessor compilation already done); it is
/// emitted as the component's single `styles` entry when non-empty.
///
/// Pipeline (identical to the back half of the `.treaty` path):
/// ```text
/// treaty_ivy::ml_parser::parse(template_html)        -> HTML AST
/// html_ast_to_render3_ast                          -> r3 AST
/// resolve_template_dependencies(imports, template) -> auto dependencies
/// compile_component_from_metadata                  -> ɵɵdefineComponent
/// build_module                                     -> runnable ES module
/// ```
pub fn compile_from_parts(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
) -> CompiledComponent {
    compile_from_parts_with_directives(class_name, javascript, template_html, styles, file_name, &[])
}

/// Like [`compile_from_parts`], but with an additional set of directive class names that the
/// front-end resolved by selectorless auto-import (e.g. the JSX directive syntaxes lowered in
/// [`crate::jsx::directives`]).
///
/// These names are merged into the component's `dependencies` in addition to the component/directive
/// references the render3 binder discovers in the template. A directive applied via an attribute
/// (`use:tooltip`, `<input Autofocus/>`, `*highlight`) lowers to plain attribute markup that the
/// instruction parser accepts but that the selectorless binder cannot itself attribute back to a
/// class — so the class names are threaded explicitly here. Each name still only becomes a
/// dependency when it was actually applied in the template (the front-end only collects applied
/// directives), preserving the "unused imports are not emitted" contract. Names are deduplicated and
/// appended after the binder-resolved dependencies, in first-seen order.
pub fn compile_from_parts_with_directives(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
    extra_directives: &[String],
) -> CompiledComponent {
    compile_from_parts_inner(
        class_name,
        javascript,
        template_html,
        styles,
        file_name,
        extra_directives,
        None,
        // OnPush is opt-in (the signal modernizer); the default authoring path emits Angular's
        // omitted `Default` strategy.
        false,
    )
    .0
}

/// Like [`compile_from_parts_with_directives`], but ALSO emits an additive Source Map v3 JSON
/// alongside the compiled module.
///
/// The map is produced through render3's source-map emitter
/// ([`treaty_ivy::output::emitter::emit_expression_with_map`]) for the lowered `ɵɵdefineComponent`
/// expression, embedding `source_content` as the map's `sourcesContent[0]` and `source_name` as
/// its `sources[0]`. `source_content` is the ORIGINAL authoring source text (the verbatim
/// `.treaty` / `.tjsx` file), so the client map carries the author's source — exactly as the base
/// `@Component` `.ts` path does via [`treaty_ivy::source_compile::compile_component_source_with_map`].
///
/// The returned `code` is byte-identical to [`compile_from_parts_with_directives`] (the map is
/// additive and never reprints the module). The second tuple element is `None` only when the
/// emitter produced an empty map.
pub fn compile_from_parts_with_directives_and_map(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
    extra_directives: &[String],
    source_name: &str,
    source_content: &str,
) -> (CompiledComponent, Option<String>) {
    compile_from_parts_inner(
        class_name,
        javascript,
        template_html,
        styles,
        file_name,
        extra_directives,
        Some((source_name, source_content)),
        // OnPush is opt-in (the signal modernizer); the default authoring path emits Angular's
        // omitted `Default` strategy.
        false,
    )
}

/// Shared implementation behind [`compile_from_parts_with_directives`] and
/// [`compile_from_parts_with_directives_and_map`].
///
/// When `source_map` is `Some((source_name, source_content))` the lowered `ɵɵdefineComponent`
/// expression is emitted through render3's `emit_expression_with_map`, threading out the additive
/// v3 map (with `source_content` embedded as `sourcesContent[0]`). When `None`, the plain
/// (map-free) `emit_expression` path is used and the second tuple element is `None`. Both paths
/// share the identical lowering + module assembly, so `code` is byte-identical between them.
fn compile_from_parts_inner(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
    extra_directives: &[String],
    source_map: Option<(&str, &str)>,
    on_push: bool,
) -> (CompiledComponent, Option<String>) {
    let mut errors: Vec<String> = Vec::new();

    let style_list: Vec<String> = if styles.is_empty() {
        Vec::new()
    } else {
        vec![styles.to_string()]
    };

    // 0. Extract signal inputs/outputs from the component-body JS chunk.
    //
    // The signal-input metadata is read from the ORIGINAL body so a `const { key: local } =
    // input<Props>()` rename still surfaces its PUBLIC `key` as the input's `binding_property_name`
    // (see [`extract_io`] → [`collect_destructured_inputs`]). The body EMITTED below is the expanded
    // form (each destructured property lowered to an individual `const local = input(<default>)`), so
    // the runtime creates one signal per input and the binding collector sees discrete `const`s. When
    // the body declares no `input<>()` destructure the expansion is a byte-for-byte passthrough, so
    // every existing `.treaty` source compiles exactly as before.
    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    extract_io(javascript, &mut inputs, &mut outputs);
    let is_signal = inputs.iter().any(|(_, m)| m.is_signal);

    let expanded_javascript = expand_input_destructures(javascript);
    let javascript = expanded_javascript.as_str();

    // 1. Template HTML -> HTML AST.
    let parse_result = treaty_ivy::ml_parser::parse(template_html, "template.html");
    for e in &parse_result.errors {
        errors.push(e.msg.clone());
    }

    // 2. HTML AST -> r3_ast. The binder resolves selectorless component refs by class name and
    //    collects dependencies automatically (no manual imports).
    let mut binding_parser = BindingParser::new();
    let r3 = html_ast_to_render3_ast(
        &parse_result.root_nodes,
        &mut binding_parser,
        Render3ParseOptions::default(),
    );
    for e in &r3.errors {
        errors.push(e.msg.clone());
    }

    // 2b. AUTO-IMPORT: resolve template dependencies from usage. The candidate set is the JS
    // chunk's imported identifiers; those actually referenced as `<Foo>` / `@Foo` / `<foo>` in the
    // template (via the selectorless binder) become the component's `dependencies`. Unused imports
    // are not emitted — mirroring `treat-to-ivy.ts`, but via the AST + binder rather than regex.
    let candidates = collect_imported_names(javascript);
    let selectorless_nodes = treaty_ivy::compile::parse_template_selectorless(template_html);
    let mut declarations =
        treaty_ivy::compile::resolve_template_dependencies(&candidates, &selectorless_nodes);

    // Append the front-end-resolved directive dependencies (the JSX directive syntaxes). These were
    // applied as plain attribute markup, which the selectorless binder cannot attribute back to a
    // class, so they are merged in here. Skip any already present (e.g. a directive also written as
    // a `<Foo>` selectorless tag) to keep the dependency list unique.
    for name in extra_directives {
        let already = declarations.iter().any(|d| {
            matches!(&d.ty.kind, treaty_ivy::output_ast::ExprKind::ReadVar { name: n } if n == name)
        });
        if !already {
            declarations.push(R3TemplateDependencyMetadata {
                kind: R3TemplateDependencyKind::Directive,
                ty: o::variable(name, None),
            });
        }
    }
    let has_directive_dependencies = !declarations.is_empty();

    // The dependency identifiers that are NOT imported are LOCAL declarations (e.g. a directive
    // declared in the same authoring file). Those must be hoisted to module scope by `build_module`
    // so the emitted `dependencies: [Foo]` reference — read at module scope — resolves to a real
    // declaration instead of dangling. Imported dependencies are already module-scope, so exclude
    // them. (`candidates` is the JS chunk's imported-identifier set from step 2b.)
    let local_dependency_names: Vec<String> = declarations
        .iter()
        .filter_map(|d| match &d.ty.kind {
            treaty_ivy::output_ast::ExprKind::ReadVar { name } => Some(name.clone()),
            _ => None,
        })
        .filter(|name| !candidates.iter().any(|c| c == name))
        .collect();

    // 3. Standalone component metadata.
    //
    // SELECTOR: the author declares none, so derive a kebab-case element selector from the file name
    // (the Treaty convention: `counter.tsx` → `counter`, `greeting-card.tjsx` → `greeting-card`,
    // `log-viewer.component.ts` → `log-viewer`). This is the source of truth that gives a bootstrapped
    // component a real host tag instead of Angular's `ng-component` no-selector default — while
    // sibling components still resolve selectorlessly by class name through the binder, so the
    // derived selector never has to be referenced explicitly in a template.
    // A multi-form selector (kebab/camel/Pascal) so a parent can reference this selectorless child as
    // `<greeting-card>`, `<greetingCard>` OR `<GreetingCard>` and Angular's runtime selector matcher
    // binds it whichever spelling the author used.
    let derived_selector = to_multi_selector(file_name);
    let base = R3DirectiveMetadata {
        name: class_name.to_string(),
        ty: class_ref(class_name),
        type_argument_count: 0,
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        // Filename-derived element selector (kebab/camel/Pascal multi-selector) when the author gives none.
        selector: Some(derived_selector),
        queries: Vec::new(),
        view_queries: Vec::new(),
        host: R3HostMetadata::default(),
        lifecycle: Lifecycle::default(),
        inputs,
        outputs,
        uses_inheritance: false,
        control_create: None,
        export_as: None,
        providers: None,
        is_standalone: true,
        is_signal,
        host_directives: None,
        legacy_optional_chaining: false,
    };

    let mut meta: R3ComponentMetadata<R3TemplateDependencyMetadata> = R3ComponentMetadata {
        base,
        template: ComponentTemplate {
            nodes: r3.nodes,
            ng_content_selectors: r3.ng_content_selectors,
            preserve_whitespaces: None,
        },
        declarations,
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles: style_list,
        external_styles: None,
        encapsulation: ViewEncapsulation::Emulated,
        animations: None,
        view_providers: None,
        relative_context_file_path: file_name.to_string(),
        i18n_use_external_ids: false,
        // Default to `ChangeDetectionStrategy.Default` — Angular's runtime default, which is OMITTED
        // from the emitted definition (byte-matching the @angular/compiler oracle). The
        // signals-by-default → OnPush design is now OPT-IN: OnPush is emitted only when the
        // `on_push` modernizer flag is threaded in (see `ModernizeOptions::on_push`).
        change_detection: Some(ChangeDetection::Strategy(if on_push {
            ChangeDetectionStrategy::OnPush
        } else {
            ChangeDetectionStrategy::Default
        })),
        relative_template_path: None,
        has_directive_dependencies,
        raw_imports: None,
        foreign_imports: None,
        // The `.treaty` JS chunk's imported identifiers, so a standalone SFC that pipes through an
        // imported pipe (`value | percent01`) lists that pipe class in `dependencies`.
        imported_directive_names: candidates.clone(),
    };

    // 4. Emit the definition.
    let mut template_builder = RealTemplateBuilder;
    let mut host_builder = StubHostBindingsBuilder;
    let mut pool_statements = Vec::new();
    let compiled: R3CompiledExpression = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    // Emit the lowered definition expression. When a source map was requested, route through
    // render3's `emit_expression_with_map` (byte-identical code, plus an additive v3 map embedding
    // the original authoring source as `sourcesContent`); otherwise use the plain emitter.
    let (cmp_expression, map) = match source_map {
        Some((source_name, source_content)) => {
            // The map's `file` is the generated artifact (the `.js` sibling of the authoring file);
            // `source_name` / `source_content` describe the original authoring source.
            let generated_name = generated_name_for(file_name);
            let (code, map_json) = emit_expression_with_map(
                &compiled.expression,
                &generated_name,
                source_name,
                source_content,
            );
            (code, map_or_none(map_json))
        }
        None => (emit_expression(&compiled.expression), None),
    };
    // The ConstantPool statements — hoisted nested-view `function <Comp>_Conditional_N_Template` /
    // `_For_N_Template` functions and shared `const _cN = [...]` literals the `ɵɵdefineComponent`
    // template references — MUST be emitted at module scope (the base `@Component` path emits them via
    // `extra_statements`; the SFC/JSX path dropped them, so any `.treaty`/`.tjsx` component with
    // `@if`/`@for`/`@switch` threw `<Comp>_Conditional_N_Template is not defined` at render time).
    let pool_code = if pool_statements.is_empty() {
        String::new()
    } else {
        emit_statements(&pool_statements)
    };
    let code =
        build_module(class_name, javascript, &cmp_expression, &local_dependency_names, &pool_code);
    (CompiledComponent { code, errors }, map)
}

/// Derive the generated-artifact name (`*.js`) for the map's `file` from the authoring file name,
/// preserving directory components. `"src/app.treaty"` -> `"src/app.js"`, `"counter.tjsx"` ->
/// `"counter.js"`; a name with no extension gains a `.js` suffix.
fn generated_name_for(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => format!("{stem}.js"),
        _ => format!("{file_name}.js"),
    }
}

/// Normalize render3's empty-string "no map" sentinel into `None`. render3 returns an empty `map`
/// when emission produced nothing mappable; any non-empty value is a real v3 JSON document.
fn map_or_none(map: String) -> Option<String> {
    if map.trim().is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Compile a `.treaty` SFC, handling a top-level `server { … }` block via the [`plugin`] system.
///
/// This is the server-aware wrapper around [`compile_treaty_file`]:
///   1. [`extract_server_block`] lifts any `server { … }` block out of the source.
///   2. The cleaned `client_source` compiles through [`compile_treaty_file`].
///   3. When server functions were present, the active backend plugin — the
///      [`PluginRegistry`](crate::plugin::PluginRegistry) default (axum + typesafe resource HTTP
///      client) — emits a server module + per-fn client bindings, and [`rewrite_call_sites`]
///      rewrites free references to each server fn in the client source to its plugin-provided
///      binding. The backend is never hardcoded; opting into another (e.g. `elysia-eden`) is a
///      registry-name lookup via [`compile_treaty_authoring_with`].
///
/// When no `server { … }` block is present the source compiles unchanged and `server_module` is
/// `None`.
///
/// [`plugin`]: crate::plugin
pub fn compile_treaty_authoring(source: &str, file_name: &str) -> CompiledAuthoring {
    let registry = PluginRegistry::with_defaults();
    let plugin = registry
        .default_plugin()
        .expect("registry seeded with a default backend plugin");
    compile_treaty_authoring_with(source, file_name, |fns| plugin.emit(fns))
}

/// Like [`compile_treaty_authoring`], but emits server functions through `emit` (the caller's chosen
/// backend) rather than the registry default. Used to opt into a non-default backend such as
/// `elysia-eden` (`PluginRegistry::get("elysia-eden")`).
pub fn compile_treaty_authoring_with(
    source: &str,
    file_name: &str,
    emit: impl FnOnce(&[ServerFn]) -> BackendEmit,
) -> CompiledAuthoring {
    // Lift `server { … }` blocks AND marker server fns (`'use server'` / `$$` / `'use websocket'`) out
    // of the `.treaty` SFC. The masked detection view (see [`mask_non_js_regions`]) lets the
    // parse-based marker lift see the JS regions despite the interleaved markup, so a marker server fn
    // in a `.treaty` JS chunk is extracted exactly like the `.ts`/`.tsx` paths.
    let extraction = extract_server_block_treaty(source, &mask_non_js_regions(source));

    // The map embeds the ORIGINAL authoring file text as `sourcesContent`, named by `file_name`,
    // mirroring the base `@Component` `.ts` path. The original `source` (pre-server-strip) is used
    // so the author's verbatim file is the map content; any lifted server-fn body is then redacted
    // out of that content below.
    if extraction.server_fns.is_empty() {
        let (compiled, map) =
            compile_treaty_file_with_map(&extraction.client_source, file_name, file_name, source);
        return CompiledAuthoring {
            code: compiled.code,
            server_module: None,
            errors: compiled.errors,
            map,
        };
    }

    // Rewrite free references to each server fn into its plugin-provided client binding *in the
    // client source*, before compilation, so the swap lands on the author's free `save(...)`
    // identifier rather than a lowered `ctx.save(...)` member access in the emitted output. The
    // binding text comes straight from the active plugin's `client_bindings` map — no backend path
    // is hardcoded here.
    let emit = emit(&extraction.server_fns);
    let client_source = rewrite_call_sites(&extraction.client_source, &emit.client_bindings);
    let (compiled, map) =
        compile_treaty_file_with_map(&client_source, file_name, file_name, source);

    // A lifted server fn that is a SIBLING export (a top-level `$$` / `'use server'` fn declared in the
    // `.treaty` JS chunk, not bound from the template) was removed by the lift, so an external
    // `import { name }` of it would now receive `undefined`. Re-export each such fn as its client
    // binding at module scope so the consumer transparently gets the RPC stub — the SAME wiring the
    // `@Component` `.ts` / plain-`.ts` / JSX paths apply, via the shared
    // [`crate::plugin::export_server_fn_bindings`]. A fn already rewritten in place is skipped.
    let with_bindings = crate::plugin::export_server_fn_bindings(
        &compiled.code,
        &extraction.server_fns,
        &emit.client_bindings,
    );

    // The rewritten call sites reference the real `@treaty/httpclient` resource helper the bindings
    // wrap. Prepend a real `import` of it so the compiled `.treaty` client module resolves the binding
    // at boot rather than throwing `<symbol> is not defined`. Empty when no binding symbol is named.
    let imports = crate::plugin::client_runtime_imports_for_code(&with_bindings);
    let code = if imports.is_empty() {
        with_bindings
    } else {
        format!("{imports}\n{with_bindings}")
    };

    // CLIENT PRIVACY: the map embeds the original authoring source as `sourcesContent`, which still
    // carries the verbatim `server { … }` block AND any marker server fn (`'use server'` / `$$`).
    // Redact each lifted fn out of the map's content (blanked to position-preserving whitespace) so the
    // server source never reaches the client map — the same guarantee the base `@Component` `.ts` path
    // provides. Both the stripped lifted `source` AND the `verbatim_source` (the EXACT original text,
    // directive included) are blanked: the verbatim form is what the map's `sourcesContent` embeds, so
    // a `'use server'` fn (whose `source` had the directive removed and so would not match the original)
    // is still fully redacted.
    let mut server_bodies: Vec<String> =
        extraction.server_fns.iter().map(|f| f.source.clone()).collect();
    server_bodies.extend(extraction.server_fns.iter().map(|f| f.verbatim_source.clone()));
    let map = map.map(|m| redact_server_bodies_in_map(&m, &server_bodies));

    CompiledAuthoring {
        code,
        server_module: Some(emit.server_module),
        errors: compiled.errors,
        map,
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn pascal_case_from_file_name() {
        assert_eq!(to_pascal_case("hello-world.treaty"), "HelloWorld");
        assert_eq!(to_pascal_case("my_widget.treaty"), "MyWidget");
        assert_eq!(to_pascal_case("src/foo/Bar.treaty"), "Bar");
        assert_eq!(to_pascal_case("name"), "Name");
        // A trailing `.component` segment is stripped (the Angular file convention).
        assert_eq!(to_pascal_case("log-viewer.component.ts"), "LogViewer");
        assert_eq!(to_pascal_case("counter.tsx"), "Counter");
        assert_eq!(to_pascal_case("features/greeter/greeting-card.tjsx"), "GreetingCard");
    }

    #[test]
    fn kebab_case_selector_from_file_name() {
        // The user-required Treaty convention: bare kebab basename, no `app-` prefix.
        assert_eq!(to_kebab_case("counter.tsx"), "counter");
        assert_eq!(to_kebab_case("greeting-card.tjsx"), "greeting-card");
        assert_eq!(to_kebab_case("gauge.treaty"), "gauge");
        // A trailing `.component` segment is dropped before kebab-casing.
        assert_eq!(to_kebab_case("log-viewer.component.ts"), "log-viewer");
        // Path components are dropped.
        assert_eq!(to_kebab_case("src/features/metrics/gauge.treaty"), "gauge");
        // PascalCase / camelCase / underscores collapse to kebab word breaks.
        assert_eq!(to_kebab_case("MyWidget.treaty"), "my-widget");
        assert_eq!(to_kebab_case("my_widget.treaty"), "my-widget");
        // A caps-run followed by a lowercase word breaks before that final cap
        // (`HTTPClient` -> `http-client`).
        assert_eq!(to_kebab_case("HTTPClient.tsx"), "http-client");
        // Digits attach to the adjacent lowercase run (no spurious break): `chart2d` -> `chart2d`.
        assert_eq!(to_kebab_case("chart2d.tsx"), "chart2d");
        // An all-separator / empty stem falls back to a stable default.
        assert_eq!(to_kebab_case("---.treaty"), "treaty-component");
    }

    #[test]
    fn treaty_component_derives_kebab_selector_from_file_name() {
        // No explicit selector in a `.treaty` source → the filename drives a kebab-case selector,
        // replacing Angular's `ng-component` no-selector default so the host tag is real.
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "log-viewer.component.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\"log-viewer\"")
                && out.code.contains("\"logViewer\"")
                && out.code.contains("\"LogViewer\""),
            "expected derived multi-form selector (log-viewer/logViewer/LogViewer); got: {}",
            out.code
        );
        assert!(
            !out.code.contains("ng-component"),
            "ng-component default must not survive; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_control_flow_hoists_template_fns_to_module_scope() {
        // Regression (`<Comp>_Conditional_N_Template is not defined` at render): a `.treaty`/`.tjsx`
        // component with `@if`/`@for`/`@switch` emits hoisted nested-view template functions that the
        // `ɵɵdefineComponent` template REFERENCES; those ConstantPool statements were collected but
        // never emitted by the SFC/JSX module assembler, so the reference dangled. They must now be
        // DEFINED at module scope.
        let source = "const items = [1, 2, 3];\n\
<ul>\n\
  @for (i of items; track i) { <li>{{ i }}</li> }\n\
  @if (items.length) { <p>has items</p> } @else { <p>empty</p> }\n\
</ul>";
        let out = compile_treaty_file(source, "list.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        // Every referenced control-flow template fn must ALSO be DEFINED (no dangling reference).
        for marker in ["_For_", "_Conditional_"] {
            let mut idx = 0;
            while let Some(rel) = code[idx..].find(marker) {
                let at = idx + rel;
                // Walk back to the identifier start, forward to its end → the full fn name.
                let start = code[..at].rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map(|p| p + 1).unwrap_or(0);
                let end = at + code[at..].find("_Template").map(|p| p + "_Template".len()).unwrap_or(marker.len());
                let name = &code[start..end];
                idx = end;
                if !name.ends_with("_Template") {
                    continue;
                }
                assert!(
                    code.contains(&format!("function {name}(")),
                    "control-flow template fn `{name}` is referenced but NOT defined at module scope; got: {code}"
                );
            }
        }
    }

    #[test]
    fn compile_from_parts_derives_selector_for_each_format_stem() {
        // The shared backend funnel derives the selector from `file_name` for any selectorless
        // authoring format (the `.tsx`/`.tjsx` paths reach the same `compile_from_parts`).
        // Each selectorless format derives a multi-form selector: the kebab form plus the PascalCase
        // form (single-word names collapse kebab == camel, so only kebab + Pascal differ).
        let cases = [
            ("counter.tsx", "counter", "Counter"),
            ("greeting-card.tjsx", "greeting-card", "GreetingCard"),
            ("gauge.treaty", "gauge", "Gauge"),
        ];
        for (file_name, kebab, pascal) in cases {
            let compiled = compile_from_parts(
                "Counter",
                "const x = 1;",
                "<div>{{ x }}</div>",
                "",
                file_name,
            );
            assert!(
                compiled.code.contains(&format!("\"{kebab}\""))
                    && compiled.code.contains(&format!("\"{pascal}\"")),
                "{file_name}: expected multi-form selector with `{kebab}` and `{pascal}`; got: {}",
                compiled.code
            );
        }
    }

    #[test]
    fn compiles_treaty_file_template_and_interpolation() {
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "greeting.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // Emits a real ɵɵdefineComponent with a real template fn.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("Greeting"), "class name missing; got: {code}");
        assert!(code.contains("Greeting_Template"), "no template fn; got: {code}");
        // The interpolation binds against the component context.
        assert!(
            code.contains("\u{0275}\u{0275}textInterpolate"),
            "no interpolation instruction; got: {code}"
        );
        assert!(code.contains("ctx.name"), "did not bind ctx.name; got: {code}");
    }

    #[test]
    fn emits_full_runnable_module() {
        let source = "import { foo } from './foo';\nconst name = 'World';\nfunction greet() {}\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "greeting.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // i0 core import is always present.
        assert!(
            code.contains("import * as i0 from \"@angular/core\";"),
            "no i0 import; got: {code}"
        );
        // The user's import statement is emitted verbatim.
        assert!(
            code.contains("import { foo } from './foo';"),
            "verbatim import missing; got: {code}"
        );
        // The function component wrapper.
        assert!(
            code.contains("function Greeting() {"),
            "no function component; got: {code}"
        );
        // The body is present with imports stripped.
        assert!(code.contains("const name = 'World';"), "body const missing; got: {code}");
        assert!(code.contains("function greet() {}"), "body fn missing; got: {code}");
        assert!(
            !code.contains("function Greeting() {\nimport"),
            "import leaked into body; got: {code}"
        );
        // Top-level const/function names are returned as the bindings object.
        assert!(
            code.contains("return { name, greet };"),
            "bindings return missing; got: {code}"
        );
        // ɵfac factory.
        assert!(
            code.contains("Greeting.\u{0275}fac = function Greeting_Factory(t) { return (t || Greeting)(); };"),
            "no ɵfac; got: {code}"
        );
        // ɵcmp = the ɵɵdefineComponent expression.
        assert!(
            code.contains(&format!("Greeting.\u{0275}cmp = i0.{DEFINE}")),
            "no ɵcmp = defineComponent; got: {code}"
        );
        // export default.
        assert!(
            code.contains("export default Greeting;"),
            "no export default; got: {code}"
        );
    }

    #[test]
    fn extracts_signal_input_from_js_chunk() {
        let source = "const name = input();\n<div>{{ name() }}</div>";
        let out = compile_treaty_file(source, "greeting.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The signal input is emitted into the inputs map and marked as a signal input.
        assert!(code.contains("inputs"), "no inputs map; got: {code}");
        assert!(code.contains("name"), "inputs missing 'name'; got: {code}");
    }

    #[test]
    fn treaty_input_object_destructure_lowers_each_property_to_an_input() {
        // FEATURE: object destructuring → inputs. `const { name, age = 0, label: caption } =
        // input<Props>()` lowers each destructured property to an INDIVIDUAL component input —
        // handling shorthand, a default, and a rename — and the runtime body declares one
        // `input(<default>)` signal per property.
        let source = "const { name, age = 0, label: caption } = input<Props>();\n\
<div>{{ name() }} {{ age() }} {{ caption() }}</div>\n";
        let out = compile_treaty_file(source, "card.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The body emits one `input()` per property (no surviving object destructure), so the runtime
        // creates a distinct signal for each input.
        assert!(
            code.contains("const name = input()"),
            "no expanded `name` input; got: {code}"
        );
        assert!(
            code.contains("const age = input(0)"),
            "default not carried into the expanded `age` input; got: {code}"
        );
        assert!(
            code.contains("const caption = input()"),
            "rename local `caption` not expanded; got: {code}"
        );
        assert!(
            !code.contains("const { name"),
            "the object destructure survived into the client body; got: {code}"
        );

        // Each property is registered as a component input. The renamed property keeps the PUBLIC
        // key `label` as its binding name (a parent binds `<card label="…">`), with the runtime
        // signal/class-property name `caption`.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(parsed.errors.is_empty(), "client did not parse: {:?}\n{code}", parsed.errors);
        for needle in ["name", "age", "caption", "label"] {
            assert!(code.contains(needle), "inputs map missing `{needle}`; got: {code}");
        }
    }

    #[test]
    fn treaty_input_destructure_only_fires_for_input_initializer() {
        // A plain (non-`input`) object destructure is NOT touched — only `input<>()`/`model<>()`
        // destructures expand. `const { x, y } = obj;` survives verbatim, so unrelated destructuring
        // is never disturbed (a byte-for-byte passthrough).
        let before = "const obj = { x: 1, y: 2 };\nconst { x, y } = obj;\n";
        assert_eq!(
            expand_input_destructures(before),
            before,
            "a non-`input` object destructure must be left untouched"
        );
        assert!(
            collect_destructured_inputs(before).is_empty(),
            "a non-`input` object destructure must yield no inputs"
        );
    }

    #[test]
    fn compiles_realistic_sfc_layout() {
        // The real REPL `.treaty` layout: a leading <style> CSS block, top-level JS
        // (imports + const), an HTML template region with {{ }} interpolation, and
        // trailing JS (console.log).
        let source = "<style>\n  .name { color: purple; }\n</style>\n\
import { input } from '@angular/core'\n\
const name = input('name')\n\
<div class=\"name\">{{ name() }}</div>\n\
console.log('hi')\n";
        let out = compile_treaty_file(source, "treat-example.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // Valid, parseable ES module (no stray '<', no EOF errors).
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "module did not parse as valid JS: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // The defineComponent definition is present.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The user import is emitted at MODULE TOP, before the function wrapper — never inside it.
        let import_idx = code
            .find("import { input } from '@angular/core'")
            .expect("user import missing");
        let fn_idx = code.find("function TreatExample() {").expect("no fn wrapper");
        assert!(
            import_idx < fn_idx,
            "user import is not above the function wrapper; got: {code}"
        );
        assert!(
            !code.contains("function TreatExample() {\nimport"),
            "import leaked into the function body; got: {code}"
        );

        // The JS *body* (inside the function wrapper) must contain no raw template markup.
        let body = &code[fn_idx..];
        let body = &body[..body.find("\nreturn {").unwrap_or(body.len())];
        assert!(
            !body.contains('<'),
            "raw template markup leaked into JS body; got body: {body}"
        );

        // Template markup went to render3, and the trailing JS stayed in the body.
        assert!(code.contains("ctx.name"), "template did not bind ctx.name; got: {code}");
        assert!(body.contains("console.log('hi')"), "trailing JS missing from body; got: {code}");
        assert!(code.contains("styles"), "no styles emitted; got: {code}");
    }

    #[test]
    fn compiles_treaty_without_template_wrapper_interleaving_ts_and_html() {
        // FIX #2: NO <template> wrapper. TS, then an HTML element, then more TS, then more HTML —
        // the HTML regions become the component template; the TS stays in the component body.
        let source = "const greeting = 'Hi';\n\
<header>{{ greeting }}</header>\n\
const footerText = 'Bye';\n\
<footer>{{ footerText }}</footer>\n";
        let out = compile_treaty_file(source, "page.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // Both HTML elements became part of the template (header + footer rendered as DOM).
        assert!(code.contains("\"header\""), "header element missing from template; got: {code}");
        assert!(code.contains("\"footer\""), "footer element missing from template; got: {code}");
        // Both interpolations bind against the component context.
        assert!(code.contains("ctx.greeting"), "first interpolation not bound; got: {code}");
        assert!(code.contains("ctx.footerText"), "second interpolation not bound; got: {code}");
        // The TS bodies are kept (returned bindings include both consts).
        assert!(
            code.contains("const greeting = 'Hi';"),
            "leading TS body missing; got: {code}"
        );
        assert!(
            code.contains("const footerText = 'Bye';"),
            "interleaved TS body missing; got: {code}"
        );
    }

    #[test]
    fn compiles_treaty_with_optional_template_wrapper() {
        // FIX #2: a file that DID use <template> still works — the wrapper is unwrapped and only its
        // inner markup becomes the template (no literal <template> element in the output).
        let source = "const name = 'World';\n\
<template>\n  <div>{{ name }}</div>\n</template>\n";
        let out = compile_treaty_file(source, "wrapped.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("ctx.name"), "interpolation not bound; got: {code}");
        // The wrapper itself must NOT appear as a rendered element.
        assert!(
            !code.contains("\"template\""),
            "the <template> wrapper leaked into the rendered template; got: {code}"
        );
    }

    #[test]
    fn compiles_treaty_with_non_ascii_without_panicking() {
        // FIX #1: non-ASCII in the macro, body, template text, attribute, interpolation and <style>
        // must compile without a mid-UTF-8-char byte-slice panic, and the text must survive.
        let source = "```\nreturn { saludo: 'Hola caf\u{00e9} \u{1F680}' };\n```\n\
const titulo = 'na\u{00ef}ve \u{1F600}';\n\
<section title=\"caf\u{00e9} \u{1F4A1}\">na\u{00ef}ve \u{1F680} {{ $macro.saludo }} \u{2013} {{ titulo }}</section>\n\
<style>/* caf\u{00e9} \u{1F680} */ .a { content: \"\u{00e9}\"; }</style>\n";
        let out = compile_treaty_file(source, "intl.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The macro ran and its non-ASCII data was injected.
        assert!(
            code.contains("Hola caf\u{00e9} \u{1F680}"),
            "macro non-ASCII data missing; got: {code}"
        );
        // The non-ASCII attribute survived into the template.
        assert!(
            code.contains("caf\u{00e9} \u{1F4A1}"),
            "non-ASCII attribute lost; got: {code}"
        );
    }

    #[test]
    fn auto_imports_used_component_into_dependencies() {
        // The author imports `Foo` and uses `<Foo>` in the template, with NO manual imports array.
        // `Foo` must land in the emitted `dependencies`; the unused `Bar` import must NOT.
        let source = "import { Foo } from './foo';\n\
import { Bar } from './bar';\n\
<div><Foo></Foo></div>";
        let out = compile_treaty_file(source, "host.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("dependencies"), "no dependencies array; got: {code}");
        // The dependencies array references Foo (the used import).
        assert!(
            code.contains("dependencies: [Foo]") || code.contains("dependencies:[Foo]"),
            "Foo not in dependencies array; got: {code}"
        );
        // Bar is imported verbatim at module top but, being unused, is NOT in dependencies.
        assert!(
            !code.contains("[Bar]") && !code.contains("Bar]") && !code.contains("[Foo, Bar"),
            "unused import Bar leaked into dependencies; got: {code}"
        );
    }

    #[test]
    fn unused_import_not_added_to_dependencies_treaty() {
        let source = "import { Foo } from './foo';\n<div>hi</div>";
        let out = compile_treaty_file(source, "host.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            !code.contains("dependencies"),
            "dependencies emitted for an unused import; got: {code}"
        );
    }

    #[test]
    fn treaty_macro_imports_and_server_block_emit_valid_js_on_raw_entry() {
        // A `.treaty` combining (1) a top ```-fenced macro block, (2) `import` declarations,
        // (3) a `server { … }` block, and (4) TS-typed body code is the greeter.treaty shape that
        // crashed the dev-serve esbuild type-strip with `Unexpected "{"`. The cause: the raw
        // (server-UNAWARE) entry left the `server { … }` block inside the synthesized component
        // function, which made the body JS chunk fail to parse, so the body's `import` declarations
        // leaked verbatim INTO the function (illegal). The raw entry must now lift the server block
        // and hoist imports to module scope so the emit is valid JS.
        let source = "```\nconst palette = ['#000']\nconst macroMeta = { palette }\n```\n\
import { signal, computed } from '@angular/core'\n\
import { type Greeting } from './greeting.types'\n\
const name = signal('Ada')\n\
const greeting = signal<Greeting | null>(null)\n\
server {\n\
  async function greet(who: string): Promise<Greeting> { return { text: who }; }\n\
}\n\
async function sayHello(): Promise<void> { greeting.set(await greet(name())); }\n\
<section><h2>{{ name() }}</h2></section>\n";

        // The RAW (server-unaware) entry that the dev-serve `.treaty` path historically used.
        let out = compile_treaty_file(source, "greeter.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // No raw `server { … }` statement survives into the client module.
        assert!(
            !code.contains("server {"),
            "raw server block leaked into client module; got: {code}"
        );

        // Imports are at MODULE scope (before the component function), never inside it.
        let fn_idx = code.find("function Greeter() {").expect("no component fn wrapper");
        let signal_import = "import { signal, computed } from '@angular/core'";
        let import_idx = code.find(signal_import).expect("signal import missing");
        assert!(
            import_idx < fn_idx,
            "an import leaked into the component function body; got: {code}"
        );

        // PARSE the emit as a TS module via oxc — no stray braces, no in-function imports.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_typescript(true).with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "greeter-shaped emit did not parse as valid TS module: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // The component still lowered (defineComponent present, template bound to ctx.name).
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("ctx.name"), "template not bound; got: {code}");
    }

    #[test]
    fn compiles_treaty_file_with_styles() {
        let source = "<div>hi</div>\n<style>.box {\n color: red;\n}</style>";
        let out = compile_treaty_file(source, "boxed.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("styles"), "no styles emitted; got: {code}");
    }

    #[test]
    fn compiles_scss_style_to_css() {
        // `lang="scss"` styles are compiled by grass before being added to the component styles.
        // The SCSS variable `$c` resolves to `red`, so the emitted CSS contains `color:red`.
        let source = "<div>hi</div>\n<style lang=\"scss\"> $c: red; .x { color: $c; }</style>";
        let out = compile_treaty_file(source, "themed.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("styles"), "no styles emitted; got: {code}");
        // Compiled SCSS: variable resolved, newlines stripped → `color:red`.
        assert!(
            code.contains("color:red"),
            "scss did not compile to `color:red`; got: {code}"
        );
    }

    #[test]
    fn treaty_server_block_extracts_route_and_rewrites_call_through_default_axum() {
        // A `.treaty` SFC with a server block declaring `save`, a body that calls `save`, and a
        // template. The DEFAULT backend (axum + typesafe resource HTTP client) is applied via the
        // PluginRegistry: the client routes the call through the resource binding (not the original
        // fn, not an eden path) and a Rust/axum server module carries the POST route for `save`.
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");

        // Server module generated as a Rust/axum service with the POST route for `save`.
        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains("\"/__server/save\""),
            "no save route in axum server module; got: {server_module}"
        );
        assert!(
            server_module.contains("pub fn build_router() -> Router"),
            "no axum router builder in server module; got: {server_module}"
        );
        assert!(
            !server_module.contains("new Elysia()"),
            "default path should not emit an Elysia app; got: {server_module}"
        );

        // The compiled client routes the call through the axum typesafe client binding: an imperative
        // `fetch` POST to the server route (NOT a `resource()` wrapper, which threw NG0203 when a
        // server fn was called imperatively in an event handler outside an injection context).
        assert!(
            out.code.contains("fetch('/__server/save'") && out.code.contains("'/__server/save'"),
            "call not rewritten to the imperative axum fetch client; got: {}",
            out.code
        );
        // The imperative binding does NOT wrap in `resource()`, so the resource helper is NOT imported.
        assert!(
            !out.code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got: {}",
            out.code
        );
        assert!(
            !out.code.contains("client.__server.save.post"),
            "default path leaked the eden binding; got: {}",
            out.code
        );
        // The server fn body never reaches the client bundle.
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_server_block_AFTER_html_region_does_not_leak_into_client_or_map() {
        // F2 regression (CLIENT SECRET LEAK): a `server { … }` block placed AFTER the HTML/view
        // region used to leak its body into BOTH the client code AND the source map, because block
        // discovery ran a JS-only text scan over the raw `.treaty` source and the closing `>` of the
        // preceding tag tripped the statement-position guard. The hardened lexer's first-class
        // `server { … }` regions (R3) discover the block regardless of position, so it must extract
        // identically whether it sits before or after the markup. A `database` call stands in for the
        // secret the user flagged — it must appear in NEITHER the client bundle NOR `sourcesContent`.
        let source = "import { User } from './user';\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n\
server {\n\
  async function save(user: User) { return database.users.insert(user, SECRET_KEY); }\n\
}\n";

        let out = compile_treaty_authoring(source, "form.treaty");

        // The server body went to the backend, not the client.
        let server_module = out.server_module.expect("expected a server module for the after-HTML block");
        assert!(
            server_module.contains("\"/__server/save\""),
            "after-HTML server block not routed to the backend; got: {server_module}"
        );

        // CLIENT CODE: neither the secret nor the body leaks; the call routes through the resource client.
        assert!(
            !out.code.contains("database.users.insert") && !out.code.contains("SECRET_KEY"),
            "after-HTML server body LEAKED into client code; got: {}",
            out.code
        );
        assert!(
            out.code.contains("fetch('/__server/save'") && out.code.contains("'/__server/save'"),
            "call not rewritten to the imperative fetch client; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got: {}",
            out.code
        );
        assert_treaty_client_parses(&out.code);

        // SOURCE MAP: the verbatim block is redacted out of `sourcesContent`, so the secret cannot be
        // recovered from the client map either (the privacy guarantee, not just the bundle).
        let map = out.map.expect("expected a source map");
        assert!(
            !map.contains("database.users.insert") && !map.contains("SECRET_KEY"),
            "after-HTML server body LEAKED into the client source map; got: {map}"
        );
    }

    /// Parse `code` as an ES module and assert it has no parse errors — by building the AST, never a
    /// regex — proving the emitted `.treaty` client module is syntactically valid.
    fn assert_treaty_client_parses(code: &str) {
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let ret = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted .treaty client did not parse: {:?}\n--- code ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    /// Parse `code` and assert `name` is bound at MODULE SCOPE by a top-level `import` specifier local,
    /// read off the PARSED AST (never a regex).
    // Retained AST-based import checker (server-fn bindings are now imperative fetch and import no
    // resource helper, so it currently has no callers).
    #[allow(dead_code)]
    fn assert_treaty_imported_at_module_scope(code: &str, name: &str) {
        use oxc_ast::ast::ImportDeclarationSpecifier;
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let ret = JsParser::new(&allocator, code, module_type).parse();
        assert!(ret.errors.is_empty(), "client code did not parse: {code}");
        let imported = ret.program.body.iter().any(|stmt| {
            let oxc_ast::ast::Statement::ImportDeclaration(import) = stmt else { return false };
            let Some(specs) = &import.specifiers else { return false };
            specs.iter().any(|spec| {
                let local = match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => &s.local.name,
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => &s.local.name,
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => &s.local.name,
                };
                local.as_str() == name
            })
        });
        assert!(imported, "`{name}` is not imported at module scope; got:\n{code}");
    }

    #[test]
    fn unified_treaty_with_use_server_fn_extracts_binds_and_imports() {
        // MATRIX (.treaty + 'use server'): a SIBLING fn carrying a `'use server'` body directive in the
        // `.treaty` JS chunk must be extracted to the server module, body ABSENT from the client, a
        // client binding re-exported, and the resource helper imported — the SAME unified wiring every
        // front-end applies.
        let source = "export async function loadUser(id: number) { 'use server'; return db.users.find(id); }\n\
const title = 'Form';\n\
<div>{{ title }}</div>\n";
        let out = compile_treaty_authoring(source, "form.treaty");

        let server_module = out.server_module.expect("use-server fn must yield a server module");
        assert!(
            server_module.contains("db.users.find"),
            "body not in server module; got:\n{server_module}"
        );

        // VERIFY EMITTED CLIENT BY PARSING.
        assert_treaty_client_parses(&out.code);
        assert!(
            !out.code.contains("db.users.find"),
            "SECURITY: body leaked into .treaty client; got:\n{}",
            out.code
        );
        assert!(
            out.code.contains("export const loadUser ="),
            "no re-exported client binding for the lifted use-server fn; got:\n{}",
            out.code
        );
        // The imperative binding does NOT wrap in `resource()`, so the resource helper is NOT imported.
        assert!(
            !out.code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_server_block_opt_in_elysia_eden_binding() {
        // Opting into the `elysia-eden` backend by registry name yields the Eden client binding and
        // an Elysia server module instead of the default axum output.
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n";

        let registry = PluginRegistry::with_defaults();
        let elysia = registry.get("elysia-eden").expect("elysia-eden registered");
        let out = compile_treaty_authoring_with(source, "form.treaty", |fns| elysia.emit(fns));

        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains(".post('/__server/save'"),
            "no save route in Elysia server module; got: {server_module}"
        );
        assert!(
            server_module.contains("new Elysia()"),
            "no Elysia app in opt-in server module; got: {server_module}"
        );

        // The compiled client routes the call through the Eden client.
        assert!(
            out.code.contains("client.__server.save.post"),
            "call not rewritten to eden client; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_in_function_server_block_extracts_and_rewrites() {
        // A `server { … }` block nested inside a function body (brace depth > 0) in a `.treaty` SFC
        // must still be lifted and its call site rewritten — exercising the depth-agnostic block scan.
        let source = "import { User } from './user';\n\
function setup(user) {\n\
  server {\n\
    async function save(u: User) { return db.insert(u); }\n\
  }\n\
  return save(user);\n\
}\n\
<div>{{ setup }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");

        let server_module = out.server_module.expect("expected a server module for nested block");
        assert!(
            server_module.contains("\"/__server/save\""),
            "no save route in axum server module; got: {server_module}"
        );
        assert!(
            out.code.contains("'/__server/save'"),
            "call not rewritten to client binding; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_inline_use_server_fn_in_component_body_absent_from_client_and_map() {
        // FEATURE: server fn IN a component (.treaty). A `'use server'` marker fn declared INSIDE a
        // nested function body must be lifted to the backend, its call site rewritten, and its body
        // ABSENT from BOTH the client code AND the client source map — closing the inline marker-form
        // gap (the `server { … }` block form was already depth-agnostic).
        let source = "function setup(user) {\n\
  async function save(u) {\n\
    'use server';\n\
    return secretDb.insert(u);\n\
  }\n\
  return save(user);\n\
}\n\
<div>{{ setup }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");

        let server_module = out.server_module.expect("expected a server module for the inline fn");
        assert!(
            server_module.contains("\"/__server/save\""),
            "no save route in axum server module; got: {server_module}"
        );
        // The body is absent from the client code.
        assert!(
            !out.code.contains("secretDb.insert"),
            "SECURITY: inline server body leaked into the .treaty client; got: {}",
            out.code
        );
        // The body is absent from the client source map.
        let map = out.map.expect("expected a client source map");
        assert!(
            !map.contains("secretDb.insert"),
            "SECURITY: inline server body leaked into the client source map; got:\n{map}"
        );
    }

    #[test]
    fn treaty_without_server_block_carries_a_v3_map() {
        // A plain `.treaty` SFC (no server block) compiles WITH an additive v3 map whose
        // `sourcesContent` embeds the original authoring source.
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_authoring(source, "greeting.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let map = out.map.expect("expected a source map for a `.treaty` SFC");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");
        // The original authoring source is embedded as `sourcesContent` and named by the file.
        assert_eq!(value["sources"][0], serde_json::json!("greeting.treaty"), "wrong source name");
        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        assert!(
            contents.iter().any(|c| c.as_str() == Some(source)),
            "authoring source not embedded as sourcesContent; got: {map}"
        );
    }

    #[test]
    fn treaty_server_block_body_is_absent_from_client_map() {
        // CLIENT PRIVACY: a `.treaty` SFC with an inline server fn must compile to a v3 map whose
        // `sourcesContent` does NOT contain the server fn body text.
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");
        assert!(out.server_module.is_some(), "expected a server module");

        let map = out.map.expect("expected a source map for a server-block `.treaty`");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");

        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        for c in contents {
            let text = c.as_str().unwrap_or("");
            assert!(!text.contains("db.insert"), "server body leaked into map content: {text}");
            assert!(
                !text.contains("async function save"),
                "server signature leaked into map content: {text}"
            );
        }
        // The redaction preserves the surrounding client text and the file name.
        assert_eq!(value["sources"][0], serde_json::json!("form.treaty"), "wrong source name");
        let joined: String = contents
            .iter()
            .filter_map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("function onClick"), "client body lost from map: {joined}");
    }

    #[test]
    fn greeter_shaped_sfc_lowers_to_valid_client_module_and_server_module() {
        // The greeter.treaty shape that previously emitted malformed output: a `.treaty` SFC with
        // module imports WITHOUT trailing semicolons (TS-by-default), a `//`-comment directly above
        // a `server { … }` block, reactive bindings, and a handler that calls the server fn. The
        // result must be a VALID ES module — imports hoisted to module scope (never inside the fn
        // wrapper), no raw `server {` text in the client, bindings collected into the returned
        // object — plus a populated server module carrying the `greet` body.
        let source = "import { signal, computed } from '@angular/core'\n\
import { type Greeting } from './greeting.types'\n\
\n\
const name = signal('Ada')\n\
const greeting = signal<Greeting | null>(null)\n\
const headline = computed(() => greeting()?.text ?? `Say hello to ${name()}`)\n\
\n\
// Every function inside this block is server-only: extracted to a sibling module.\n\
server {\n\
\tasync function greet(who: string): Promise<Greeting> {\n\
\t\tconst text = `Hello, ${who}!`\n\
\t\treturn { text, at: Date.now() }\n\
\t}\n\
}\n\
\n\
async function sayHello(): Promise<void> {\n\
\tgreeting.set(await greet(name().trim() || 'world'))\n\
}\n\
\n\
<section class=\"greeter\">\n\
  <h2>{{ headline() }}</h2>\n\
  <button (click)=\"sayHello()\">Greet</button>\n\
</section>\n";

        let out = compile_treaty_authoring(source, "greeter.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // (b) The raw `server {` block text must NOT survive into the client module.
        assert!(
            !code.contains("server {"),
            "raw server block leaked into client; got: {code}"
        );
        // The server fn body must NOT reach the client bundle.
        assert!(
            !code.contains("Date.now()") && !code.contains("Hello, ${who}"),
            "server body leaked into client; got: {code}"
        );

        // The whole client module RE-PARSES as a valid ES module via oxc. The emitted body keeps
        // the author's TypeScript (e.g. `signal<Greeting | null>(...)`), so parse as a TS module.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "client module did not parse as valid TS module: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // (a) Imports are HOISTED to module scope, above the function wrapper — never inside it.
        let fn_idx = code.find("function Greeter() {").expect("no fn wrapper");
        let core_import_idx = code
            .find("import { signal, computed } from '@angular/core'")
            .expect("user core import missing");
        let types_import_idx = code
            .find("import { type Greeting } from './greeting.types'")
            .expect("types import missing");
        assert!(
            core_import_idx < fn_idx && types_import_idx < fn_idx,
            "an import is not above the function wrapper; got: {code}"
        );
        // No `import` statement appears anywhere inside the function body.
        let body_start = fn_idx;
        let body = &code[body_start..];
        let body = &body[..body.find("\nreturn {").unwrap_or(body.len())];
        assert!(
            !body.contains("import "),
            "import leaked into the function body; got body: {body}"
        );

        // (c) The reactive bindings are collected into the returned object (non-empty return).
        for binding in ["name", "greeting", "headline", "sayHello"] {
            assert!(
                code.contains(&format!("return {{ ")) && code.contains(binding),
                "binding `{binding}` not collected into the returned object; got: {code}"
            );
        }
        // The return object is not empty.
        assert!(
            !code.contains("return {  };") && !code.contains("return { };"),
            "bindings return is empty; got: {code}"
        );

        // A server module IS produced and carries the `greet` route + body.
        let server_module = out.server_module.expect("expected a server module for greet");
        assert!(
            server_module.contains("\"/__server/greet\""),
            "no greet route in server module; got: {server_module}"
        );

        // The component definition is otherwise intact.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("ctx.headline"), "template did not bind headline; got: {code}");
    }

    #[test]
    fn treaty_without_server_block_has_no_server_module() {
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_authoring(source, "greeting.treaty");
        assert!(out.server_module.is_none(), "unexpected server module");
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn executes_macro_block_without_breaking_compilation() {
        // A top-level fenced macro block is EXECUTED (its computed value injected); the macro
        // SOURCE itself must not leak into the JS body or the template. This statement-only macro
        // produces no value, so it injects `const $macro = null;` and the component still compiles.
        let source = "```\nconst x = 1;\n```\nconst name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "withmacro.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // Component still compiles.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("function Withmacro() {"), "no fn wrapper; got: {code}");
        // The macro SOURCE is NOT emitted into the module — only its computed value is.
        assert!(
            !code.contains("const x = 1;"),
            "macro source leaked into output; got: {code}"
        );
        // The macro injected its (empty) result as `$macro`.
        assert!(code.contains("const $macro = null;"), "macro value not injected; got: {code}");
        // The real component body and template are intact.
        assert!(code.contains("const name = 'World';"), "body const missing; got: {code}");
        assert!(code.contains("ctx.name"), "template did not bind ctx.name; got: {code}");
    }

    #[test]
    fn macro_data_is_injected_and_bindable_in_template() {
        // A macro that produces data: its computed value is injected as `const $macro = {...};`,
        // exposed as a component binding, and bindable in the template — while the macro SOURCE
        // (the `title`/`count` computation) never reaches the emitted module.
        let source = "```\n\
const title: string = 'Hello from macro';\n\
const count: number = 2 * 21;\n\
return { title, count };\n\
```\n\
<h1>{{ $macro.title }}</h1>\n";
        let out = compile_treaty_file(source, "page.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // The component compiled.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The macro's computed value is injected as a JSON object literal `const`. The macro ran
        // the arithmetic and string ops, so the literal carries the RESULTS, not the source. (Key
        // order in the JSON encoding is not significant, so each field is checked individually.)
        assert!(code.contains("const $macro = {"), "macro data literal not injected; got: {code}");
        assert!(
            code.contains("\"title\":\"Hello from macro\""),
            "macro `title` result not injected; got: {code}"
        );
        assert!(
            code.contains("\"count\":42"),
            "macro `count` result not injected; got: {code}"
        );

        // The macro SOURCE never leaks (no `return { title, count }`, no `2 * 21`).
        assert!(!code.contains("2 * 21"), "macro source leaked; got: {code}");
        assert!(
            !code.contains("return { title, count }"),
            "macro source leaked; got: {code}"
        );
        assert!(
            !code.contains(": string") && !code.contains(": number"),
            "macro TS annotations leaked; got: {code}"
        );

        // `$macro` is collected into the component's returned bindings, so the template context
        // sees it.
        assert!(
            code.contains("$macro"),
            "macro binding not returned to component context; got: {code}"
        );
        // The template binds the macro data against the component context.
        assert!(
            code.contains("ctx.$macro") || code.contains("ctx.$macro.title"),
            "template did not bind macro data; got: {code}"
        );

        // The emitted module is valid, parseable JS.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "module did not parse as valid JS: {:?}\n--- code ---\n{code}",
            parsed.errors
        );
    }

    // ─────────────────────── R1: top-level control flow lowers to Ivy ───────────────────────
    //
    // The R1 fix: a control-flow block that is NOT wrapped in a host element used to be a bare marker
    // whose body was dropped, so the block never reached the template. It now lowers to real
    // control-flow Ivy. Each emit below is VERIFIED BY PARSING (oxc) — never a regex over the emit.

    #[test]
    fn top_level_if_lowers_to_conditional_ivy() {
        // A bare top-level `@if` (no surrounding element) must reach the template and lower to the
        // `ɵɵconditional` instruction family — the precise block the R1 audit said was silently lost.
        let source = "const ready = true;\n@if (ready) {\n  <p>Go</p>\n}";
        let out = compile_treaty_file(source, "panel.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // The emit re-parses as a valid module (proves the block did not corrupt the output).
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(parsed.errors.is_empty(), "emit did not parse: {:?}\n{code}", parsed.errors);

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The `@if` lowered to the conditional instruction family (create + update).
        assert!(
            code.contains("\u{0275}\u{0275}conditionalCreate") || code.contains("\u{0275}\u{0275}conditional"),
            "top-level @if did not lower to ɵɵconditional; got: {code}"
        );
        // The body content reached the template.
        assert!(code.contains("\"p\""), "the @if body element was lost; got: {code}");
    }

    #[test]
    fn top_level_if_else_both_branches_lower() {
        // `@if (…) { … } @else { … }` — both branches reach the template and chain into the same
        // conditional region.
        let source = "const ok = false;\n@if (ok) {\n  <p>yes</p>\n} @else {\n  <span>no</span>\n}";
        let out = compile_treaty_file(source, "branch.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(parsed.errors.is_empty(), "emit did not parse: {:?}\n{code}", parsed.errors);

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}conditional"),
            "@if/@else did not lower to ɵɵconditional; got: {code}"
        );
        // BOTH branch elements reached the template.
        assert!(code.contains("\"p\"") && code.contains("\"span\""), "an @if/@else branch was lost; got: {code}");
    }

    #[test]
    fn top_level_for_lowers_to_repeater_ivy() {
        // A bare top-level `@for (… ; track …) { … } @empty { … }` lowers to the repeater instruction
        // family, binds the loop variable against the component context, and keeps both bodies.
        let source = "const items = signal([1, 2, 3]);\n\
@for (item of items(); track item) {\n  <li>{{ item }}</li>\n} @empty {\n  <li>none</li>\n}";
        let out = compile_treaty_file(source, "list.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(parsed.errors.is_empty(), "emit did not parse: {:?}\n{code}", parsed.errors);

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            "top-level @for did not lower to ɵɵrepeater; got: {code}"
        );
        // The loop body element + the @empty body element both reached the template.
        assert!(code.contains("\"li\""), "the @for body element was lost; got: {code}");
        // The component body (the `items` signal) is still collected as a binding.
        assert!(code.contains("const items ="), "the component-body TS was lost; got: {code}");
    }

    #[test]
    fn control_flow_interleaved_with_html_and_trailing_ts_all_survive() {
        // A top-level `@if` BETWEEN an HTML element and trailing TS: the element, the control-flow
        // block, and the trailing TS must all survive — the block no longer swallows or drops siblings.
        let source = "<h1>Title</h1>\n@if (show()) {\n  <p>body</p>\n}\nconst show = signal(true);";
        let out = compile_treaty_file(source, "page.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(parsed.errors.is_empty(), "emit did not parse: {:?}\n{code}", parsed.errors);

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The leading <h1>, the @if body <p>, and the trailing TS binding are all present.
        assert!(code.contains("\"h1\""), "leading HTML lost; got: {code}");
        assert!(code.contains("\"p\""), "the @if body was lost; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}conditional"),
            "the interleaved @if did not lower to ɵɵconditional; got: {code}"
        );
        assert!(code.contains("const show ="), "trailing TS lost; got: {code}");
    }

    #[test]
    fn server_block_and_top_level_control_flow_coexist() {
        // R1 + R3 together: a `.treaty` SFC with a first-class `server { … }` block AND a TOP-LEVEL
        // `@if`/`@else` control-flow region (not wrapped in a host element). The server body is lifted
        // to the server module (absent from the client), and the control-flow block lowers to
        // `ɵɵconditional` in the client template. Verified by parsing the emit.
        let source = "import { signal } from '@angular/core'\n\
const open = signal(false)\n\
server {\n\
  async function persist(v: number) { return db.save(v); }\n\
}\n\
function toggle() { open.set(!open()); persist(1); }\n\
@if (open()) {\n  <p>open</p>\n} @else {\n  <p>closed</p>\n}\n";

        let out = compile_treaty_authoring(source, "panel.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // The client re-parses as a valid TS module.
        assert_treaty_client_parses(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // R3: the server body was lifted to a server module and is ABSENT from the client.
        let server_module = out.server_module.expect("server block must yield a server module");
        assert!(
            server_module.contains("\"/__server/persist\""),
            "no persist route in the server module; got: {server_module}"
        );
        assert!(!code.contains("db.save"), "server body leaked into the client; got: {code}");

        // R1: the top-level @if/@else lowered to control-flow Ivy, with both branches present.
        assert!(
            code.contains("\u{0275}\u{0275}conditional"),
            "top-level @if/@else did not lower to ɵɵconditional; got: {code}"
        );
        assert!(code.contains("\"p\""), "the control-flow branch body was lost; got: {code}");
    }

    /// Read an everything-app example `.treaty` file relative to this crate's manifest dir
    /// (`libs/authoring/rust`), so the gate runs against the REAL authored fixtures the lexer must
    /// keep working — not a synthetic copy that could drift from the shipped examples.
    fn read_example(rel_from_repo_root: &str) -> String {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // `libs/authoring/rust` -> repo root is three levels up.
        let repo_root = manifest
            .ancestors()
            .nth(3)
            .expect("crate manifest is nested under the repo root");
        let path = repo_root.join(rel_from_repo_root);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read example fixture {}: {e}", path.display()))
    }

    #[test]
    fn example_treaty_files_compile_to_define_component() {
        // R2 GATE: the four shipped everything-app `.treaty` examples (gauge under features/metrics,
        // greeter, todo-list) must still compile through the real `compile_treaty_authoring` seam to a
        // valid, re-parseable Ivy `ɵɵdefineComponent` module after the balanced-scanner rewrite. These
        // files exercise the rough edges directly: multi-line `computed(() => { … })` arrows
        // (gauge/greeter), a `server { … }` block with a `` `Hello, ${who}!` `` template literal
        // (greeter), interleaved TS/HTML/`<style>` with `@if`/`@for`, and `<input … />` elements.
        let fixtures = [
            ("examples/everything-app/src/features/metrics/gauge.treaty", "gauge.treaty"),
            ("examples/everything-app/src/features/greeter/greeter.treaty", "greeter.treaty"),
            ("examples/everything-app/src/components/todo-list.treaty", "todo-list.treaty"),
        ];
        for (rel, file_name) in fixtures {
            let source = read_example(rel);
            let out = compile_treaty_authoring(&source, file_name);
            assert!(
                out.errors.is_empty(),
                "{file_name}: compile reported errors: {:?}",
                out.errors
            );
            // VERIFY EMITTED CODE BY PARSING (oxc), never a regex over the emit.
            assert_treaty_client_parses(&out.code);
            assert!(
                out.code.contains(DEFINE),
                "{file_name}: emit is not a component (no ɵɵdefineComponent); got:\n{}",
                out.code
            );
            // No raw Angular `@Component(` decorator node survives (AOT, no JIT).
            assert!(
                !out.code.contains("@Component("),
                "{file_name}: a raw @Component decorator survived in the emit; got:\n{}",
                out.code
            );
        }
    }

    #[test]
    fn greeter_server_block_template_literal_does_not_leak_to_client() {
        // The greeter's `server { … }` block holds `const text = `Hello, ${who}!`` — a multi-line
        // template-literal body. The balanced scanner must keep that literal whole inside the lifted
        // server fn so it is extracted to the server module and NEVER reaches the client (the precise
        // privacy contract the everything-app source-validate gate asserts for greeter.treaty).
        let source = read_example("examples/everything-app/src/features/greeter/greeter.treaty");
        let out = compile_treaty_authoring(&source, "greeter.treaty");

        let server_module = out
            .server_module
            .as_deref()
            .expect("greeter's server block must yield a server module");
        assert!(
            server_module.contains("const text = `Hello, ${who}!`"),
            "server-fn body not extracted to the server module; got:\n{server_module}"
        );
        // The body / template literal must be ABSENT from the client (verified emit re-parses too).
        assert_treaty_client_parses(&out.code);
        assert!(
            !out.code.contains("const text = `Hello, ${who}!`"),
            "SECURITY: server-fn body template literal leaked into the greeter client; got:\n{}",
            out.code
        );
    }
}
