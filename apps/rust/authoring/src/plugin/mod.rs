//! Backend-agnostic compiler plugin system + `server { … }` block extraction.
//!
//! Treaty lets an author colocate server-only logic in a component source by wrapping it in a
//! top-level `server { … }` block. The compiler must lift those functions out of the client bundle
//! and hand them to a *backend plugin* (Elysia/Eden being the reference implementation) which emits
//! the actual server module and the client-side call replacements.
//!
//! This module is intentionally backend-agnostic:
//!   * [`extract_server_block`] is a pure text/AST pre-pass: it removes the `server { … }` block
//!     from the source and parses the functions declared inside it via OXC.
//!   * [`BackendPlugin`] is the trait every backend implements; it turns the extracted [`ServerFn`]s
//!     into a [`BackendEmit`] (server module text + per-fn client-call bindings).
//!   * [`rewrite_call_sites`] is an identifier-aware rewrite of the client source that swaps each
//!     extracted function name for its client binding expression.
//!
//! The reference [`ElysiaEdenPlugin`] wires these together for an Elysia/Eden backend.

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, Statement};
use oxc_parser::Parser as JsParser;
use oxc_span::{GetSpan, SourceType};

/// The two-word directive that marks a top-level function as server-only.
const USE_SERVER_DIRECTIVE: &str = "use server";

mod elysia;
pub use elysia::ElysiaEdenPlugin;

// ---------------------------------------------------------------------------
// Backend-agnostic data model.
// ---------------------------------------------------------------------------

/// A single typed parameter of a server function: its binding name and optional TypeScript type
/// annotation text (sliced verbatim from the source, e.g. `"User"` or `"{ id: number }"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerParam {
    pub name: String,
    pub ty: Option<String>,
}

/// A server-only function lifted out of a `server { … }` block.
///
/// `source` is the verbatim function text as written by the author (used by backends that re-emit
/// the body); `params`/`return_type`/`is_async` are the parsed signature, so backends can generate
/// typed routes without re-parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFn {
    pub name: String,
    pub source: String,
    pub params: Vec<ServerParam>,
    pub return_type: Option<String>,
    pub is_async: bool,
}

/// The result of [`extract_server_block`]: the client source with the `server { … }` block removed,
/// plus the functions that were declared inside it. When no block is present, `client_source` is the
/// input unchanged and `server_fns` is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerExtraction {
    pub client_source: String,
    pub server_fns: Vec<ServerFn>,
}

/// What a [`BackendPlugin`] produces from a set of [`ServerFn`]s.
///
/// `server_module` is the generated backend code (one string, ready to write to a file).
/// `client_bindings` maps each server fn name to the client-side expression that replaces calls to
/// it (e.g. `save` -> `client.save` for an Eden treaty client).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackendEmit {
    pub server_module: String,
    pub client_bindings: HashMap<String, String>,
}

/// A backend code generator. Implementations turn extracted server functions into a
/// [`BackendEmit`]. Backends are selected from the [`PluginRegistry`] by `name`.
pub trait BackendPlugin {
    /// Stable identifier for this backend (e.g. `"elysia"`), used for registry lookup.
    fn name(&self) -> &str;

    /// Generate the server module + client bindings for `fns`.
    fn emit(&self, fns: &[ServerFn]) -> BackendEmit;
}

/// A registry of available backend plugins with a default selection.
///
/// The first plugin registered becomes the default; [`PluginRegistry::with_defaults`] seeds it with
/// the reference [`ElysiaEdenPlugin`].
pub struct PluginRegistry {
    plugins: Vec<Box<dyn BackendPlugin>>,
}

impl PluginRegistry {
    /// An empty registry (no plugins, no default).
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    /// A registry preloaded with the reference [`ElysiaEdenPlugin`] as the default backend.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(ElysiaEdenPlugin));
        registry
    }

    /// Register a backend plugin. The first one registered is the default.
    pub fn register(&mut self, plugin: Box<dyn BackendPlugin>) {
        self.plugins.push(plugin);
    }

    /// Look up a backend by name.
    pub fn get(&self, name: &str) -> Option<&dyn BackendPlugin> {
        self.plugins
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }

    /// The default backend (the first one registered), if any.
    pub fn default_plugin(&self) -> Option<&dyn BackendPlugin> {
        self.plugins.first().map(|p| p.as_ref())
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ---------------------------------------------------------------------------
// `server { … }` block extraction.
// ---------------------------------------------------------------------------

/// Find a top-level `server { … }` block, remove it from `source`, and parse the functions declared
/// inside it.
///
/// The scan is a text pre-pass that is aware of strings, template literals, line/block comments, and
/// nested braces, so a `{` inside `"…"`, `` `…` ``, `/* … */`, or `// …` never ends the block early.
/// When no top-level `server { … }` block is present the source is returned unchanged with an empty
/// `server_fns` list.
pub fn extract_server_block(source: &str) -> ServerExtraction {
    // 1. Lift any explicit `server { … }` block first.
    let (mut client_source, mut server_fns) =
        if let Some((block_start, body_start, body_end, block_end)) = find_server_block(source) {
            // The function declarations live in the brace body (exclusive of the braces themselves).
            let body = &source[body_start..body_end];
            let fns = parse_server_fns(body);

            // Client source = everything outside the `server { … }` block.
            let mut client = String::with_capacity(source.len());
            client.push_str(&source[..block_start]);
            client.push_str(&source[block_end..]);
            (client, fns)
        } else {
            (source.to_string(), Vec::new())
        };

    // 2. Lift the remaining top-level marker forms (`'use server'` directive, `name$$` suffix) from
    //    whatever client source survived step 1, removing their declarations as we go.
    let (rewritten, marker_fns) = extract_marker_fns(&client_source);
    client_source = rewritten;
    server_fns.extend(marker_fns);

    ServerExtraction { client_source, server_fns }
}

/// Scan top-level declarations of `source` for the two marker conventions that do not use an explicit
/// `server { … }` wrapper, lift them out, and return the source with their declarations removed.
///
/// A top-level declaration is a server fn when it is either:
///   * a `function` / `const NAME = (…) => …` whose body's first statement is the bare
///     `'use server'` string directive (the directive statement is stripped from the lifted source), or
///   * a `function` / `const NAME = (…) => …` whose **name** ends in two `$` characters.
///
/// The scan parses `source` once with OXC and removes the matched declarations by byte span (highest
/// span first so earlier offsets stay valid). Only program-top-level declarations are considered.
fn extract_marker_fns(source: &str) -> (String, Vec<ServerFn>) {
    if source.trim().is_empty() {
        return (source.to_string(), Vec::new());
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, source, source_type).parse();

    let mut fns = Vec::new();
    // Byte spans of the top-level declarations we remove from the client source.
    let mut removals: Vec<(usize, usize)> = Vec::new();

    for stmt in &ret.program.body {
        if let Some((server_fn, span)) = server_fn_from_statement(source, stmt) {
            removals.push(span);
            fns.push(server_fn);
        }
    }

    let client_source = strip_spans(source, &mut removals);
    (client_source, fns)
}

/// Remove each `(start, end)` byte span from `source`. Also swallows one trailing newline after each
/// removed span so a lifted declaration does not leave a dangling blank line.
fn strip_spans(source: &str, removals: &mut Vec<(usize, usize)>) -> String {
    if removals.is_empty() {
        return source.to_string();
    }
    // Remove from the back so earlier byte offsets remain valid.
    removals.sort_by(|a, b| b.0.cmp(&a.0));
    let bytes = source.as_bytes();
    let mut out = source.to_string();
    for &(start, end) in removals.iter() {
        let mut end = end;
        if end < bytes.len() && bytes[end] == b'\r' {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'\n' {
            end += 1;
        }
        out.replace_range(start..end, "");
    }
    out
}

/// Locate a top-level `server { … }` block. Returns `(block_start, body_start, body_end, block_end)`
/// where `block_start..block_end` is the full block (`server` keyword through the closing `}`,
/// including a trailing newline if present) and `body_start..body_end` is the brace *interior*.
fn find_server_block(source: &str) -> Option<(usize, usize, usize, usize)> {
    let bytes = source.as_bytes();
    let mut i = 0usize;

    // A `server` keyword only opens a block when it is a standalone identifier (not e.g. `servery`
    // or `x.server`) at brace-depth zero, outside any string/comment.
    let mut scanner = TextScanner::new(source);
    while i < bytes.len() {
        // Advance the scanner state up to `i`, skipping strings/comments wholesale.
        if let Some(next) = scanner.skip_noncode(i) {
            i = next;
            continue;
        }

        if scanner.depth == 0 && matches_keyword(source, i, "server") {
            // After `server`, allow whitespace, then require a `{`.
            let after_kw = i + "server".len();
            let mut j = after_kw;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'{' {
                let body_start = j + 1;
                let body_end = match_close_brace(source, body_start)?;
                // close brace index = body_end (match_close_brace returns index of the `}`).
                let mut block_end = body_end + 1;
                // Swallow a single trailing newline so removal does not leave a blank line.
                if block_end < bytes.len() && bytes[block_end] == b'\r' {
                    block_end += 1;
                }
                if block_end < bytes.len() && bytes[block_end] == b'\n' {
                    block_end += 1;
                }
                return Some((i, body_start, body_end, block_end));
            }
        }

        scanner.step(i);
        i += 1;
    }
    None
}

/// Given the index just *after* an opening `{`, return the index of the matching `}`. Tracks string,
/// template-literal, and comment context so braces inside them are ignored. Returns `None` if the
/// braces are unbalanced.
fn match_close_brace(source: &str, body_start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut scanner = TextScanner::new(source);
    let mut depth = 1usize;
    let mut i = body_start;
    while i < bytes.len() {
        if let Some(next) = scanner.skip_noncode(i) {
            i = next;
            continue;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        scanner.step(i);
        i += 1;
    }
    None
}

/// Is `keyword` present at byte `i` as a whole word (not part of a larger identifier)?
fn matches_keyword(source: &str, i: usize, keyword: &str) -> bool {
    let bytes = source.as_bytes();
    if !source[i..].starts_with(keyword) {
        return false;
    }
    // Preceding char must not be an identifier char (so `myserver` does not match).
    if i > 0 {
        let prev = bytes[i - 1];
        if is_ident_byte(prev) {
            return false;
        }
    }
    // Following char must not be an identifier char (so `servery` does not match).
    let after = i + keyword.len();
    if after < bytes.len() && is_ident_byte(bytes[after]) {
        return false;
    }
    true
}

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b == b'$' || b.is_ascii_alphanumeric()
}

/// Tracks lexical context (strings, template literals, comments) while scanning JS/TS text, so the
/// brace matcher can ignore braces that live inside them. Also tracks paren/bracket/brace depth via
/// callers (see `find_server_block`, which uses `depth` for the top-level check).
struct TextScanner {
    bytes: Vec<u8>,
    /// Brace/paren/bracket nesting depth (incremented by callers as they consume code chars).
    depth: usize,
}

impl TextScanner {
    fn new(source: &str) -> Self {
        Self { bytes: source.as_bytes().to_vec(), depth: 0 }
    }

    /// If a string/template/comment begins at `i`, consume it whole and return the index just past
    /// it. Otherwise return `None` (the byte at `i` is ordinary code).
    fn skip_noncode(&self, i: usize) -> Option<usize> {
        let bytes = &self.bytes;
        let b = bytes[i];
        match b {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // Line comment: to end of line.
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'\n' {
                    j += 1;
                }
                Some(j)
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // Block comment: to `*/`.
                let mut j = i + 2;
                while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                    j += 1;
                }
                Some((j + 2).min(bytes.len()))
            }
            b'"' | b'\'' => Some(skip_quoted(bytes, i, b)),
            b'`' => Some(skip_template(bytes, i)),
            _ => None,
        }
    }

    /// Advance plain-code bracket depth for the byte at `i`. Only called when `i` is ordinary code
    /// (i.e. `skip_noncode` returned `None`).
    fn step(&mut self, i: usize) {
        match self.bytes[i] {
            b'{' | b'(' | b'[' => self.depth += 1,
            b'}' | b')' | b']' => {
                self.depth = self.depth.saturating_sub(1);
            }
            _ => {}
        }
    }
}

/// Consume a `'…'` or `"…"` string starting at `i` (the opening quote), honoring backslash escapes.
/// Returns the index just past the closing quote.
fn skip_quoted(bytes: &[u8], i: usize, quote: u8) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b if b == quote => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// Consume a `` `…` `` template literal starting at `i` (the opening backtick). `${ … }` interpolation
/// segments are skipped as nested code (including nested templates/strings). Returns the index just
/// past the closing backtick.
fn skip_template(bytes: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'`' => return j + 1,
            b'$' if j + 1 < bytes.len() && bytes[j + 1] == b'{' => {
                // Skip the `${ … }` interpolation, tracking nested braces.
                let mut depth = 1usize;
                j += 2;
                while j < bytes.len() && depth > 0 {
                    match bytes[j] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        b'`' => j = skip_template(bytes, j).wrapping_sub(1),
                        b'"' | b'\'' => j = skip_quoted(bytes, j, bytes[j]).wrapping_sub(1),
                        _ => {}
                    }
                    j += 1;
                }
            }
            _ => j += 1,
        }
    }
    bytes.len()
}

/// Parse server functions from the `server { … }` body text via OXC.
///
/// Every top-level declaration inside the block is a server fn, regardless of marker: both
/// `function` declarations and `const NAME = (…) => …` arrow-consts are lifted. For each we capture
/// the verbatim source slice, name, params (name + optional type text), return type text, and async
/// flag.
fn parse_server_fns(body: &str) -> Vec<ServerFn> {
    let mut fns = Vec::new();
    if body.trim().is_empty() {
        return fns;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, body, source_type).parse();

    for stmt in &ret.program.body {
        // Inside an explicit `server { … }` block, every declaration is server-only, so we do not
        // gate on a marker — `require_marker = false`.
        if let Some((server_fn, _span)) = build_server_fn(body, stmt, false) {
            fns.push(server_fn);
        }
    }

    fns
}

/// Decide whether a program-top-level statement is a marker-based server fn (`'use server'`
/// directive or a `$$`-suffixed name) and, if so, build it. Returns the [`ServerFn`] plus the byte
/// span of the whole declaration (so the caller can remove it from the client source).
fn server_fn_from_statement(source: &str, stmt: &Statement) -> Option<(ServerFn, (usize, usize))> {
    build_server_fn(source, stmt, true)
}

/// Build a [`ServerFn`] from a `function` declaration or a single-declarator `const NAME = (…) => …`
/// arrow-const statement. Handles both forms uniformly.
///
/// When `require_marker` is true the statement is only treated as a server fn if it carries one of
/// the markers: a `$$`-suffixed name, or a `'use server'` directive as the first body statement
/// (which is then stripped from the emitted source). When false (inside a `server { … }` block) the
/// declaration is always lifted and any leading `'use server'` directive is still stripped.
///
/// Returns the built fn and the byte span `(start, end)` of the full statement in `source`.
fn build_server_fn(
    source: &str,
    stmt: &Statement,
    require_marker: bool,
) -> Option<(ServerFn, (usize, usize))> {
    match stmt {
        Statement::FunctionDeclaration(func) => {
            let id = func.id.as_ref()?;
            let name = id.name.to_string();

            let directive = func
                .body
                .as_deref()
                .and_then(|body| leading_use_server_span(source, body));
            let has_marker = name.ends_with("$$") || directive.is_some();
            if require_marker && !has_marker {
                return None;
            }

            let span = (func.span.start as usize, func.span.end as usize);
            let src = strip_directive(source, span, directive);

            let params = collect_params(source, &func.params);
            let return_type = func.return_type.as_ref().map(|ann| span_text(source, ann.type_annotation.span()));

            Some((
                ServerFn { name, source: src, params, return_type, is_async: func.r#async },
                span,
            ))
        }
        Statement::VariableDeclaration(decl) => {
            // Only a single-declarator `const NAME = (…) => …` is a server-fn candidate.
            if decl.declarations.len() != 1 {
                return None;
            }
            let declarator = decl.declarations.first()?;
            let name = declarator.id.get_identifier_name()?.to_string();
            let Some(Expression::ArrowFunctionExpression(arrow)) = &declarator.init else {
                return None;
            };

            let directive = leading_use_server_span(source, &arrow.body);
            let has_marker = name.ends_with("$$") || directive.is_some();
            if require_marker && !has_marker {
                return None;
            }

            let span = (decl.span.start as usize, decl.span.end as usize);
            let src = strip_directive(source, span, directive);

            let params = collect_params(source, &arrow.params);
            let return_type = arrow.return_type.as_ref().map(|ann| span_text(source, ann.type_annotation.span()));

            Some((
                ServerFn { name, source: src, params, return_type, is_async: arrow.r#async },
                span,
            ))
        }
        _ => None,
    }
}

/// If the function/arrow body begins with the bare `'use server'` string directive, return its byte
/// span (`start..end`) in `source` so the caller can excise it from the lifted function text.
///
/// OXC parses leading string-literal statements as `FunctionBody::directives`, so we inspect the
/// first directive there.
fn leading_use_server_span(
    _source: &str,
    body: &oxc_ast::ast::FunctionBody,
) -> Option<(usize, usize)> {
    let directive = body.directives.first()?;
    if directive.expression.value.as_str() == USE_SERVER_DIRECTIVE {
        let span = directive.span;
        Some((span.start as usize, span.end as usize))
    } else {
        None
    }
}

/// Slice the declaration `span` out of `source` and, if a `directive` span is present, remove that
/// directive statement (plus any trailing `;`, surrounding whitespace, and one newline) from the
/// resulting text so the lifted function no longer carries the `'use server'` marker.
fn strip_directive(
    source: &str,
    span: (usize, usize),
    directive: Option<(usize, usize)>,
) -> String {
    let (start, end) = span;
    let mut text = source[start..end].to_string();
    let Some((d_start, d_end)) = directive else {
        return text;
    };
    // Translate the directive span into offsets relative to the sliced declaration text.
    let mut rel_start = d_start - start;
    let mut rel_end = d_end - start;
    let bytes = text.as_bytes();
    // Swallow a trailing `;` then trailing whitespace up to and including one newline.
    if rel_end < bytes.len() && bytes[rel_end] == b';' {
        rel_end += 1;
    }
    while rel_end < bytes.len() && (bytes[rel_end] == b' ' || bytes[rel_end] == b'\t') {
        rel_end += 1;
    }
    if rel_end < bytes.len() && bytes[rel_end] == b'\r' {
        rel_end += 1;
    }
    if rel_end < bytes.len() && bytes[rel_end] == b'\n' {
        rel_end += 1;
    }
    // Also pull back over the indentation that preceded the directive on its line.
    while rel_start > 0 && (bytes[rel_start - 1] == b' ' || bytes[rel_start - 1] == b'\t') {
        rel_start -= 1;
    }
    text.replace_range(rel_start..rel_end, "");
    text
}

/// Extract the parameter list (name + optional verbatim type text) for a function/arrow signature.
fn collect_params(source: &str, params: &oxc_ast::ast::FormalParameters) -> Vec<ServerParam> {
    let mut out = Vec::new();
    for param in &params.items {
        let name = param
            .pattern
            .get_identifier_name()
            .map(|n| n.to_string())
            .unwrap_or_default();
        let ty = param
            .type_annotation
            .as_ref()
            .map(|ann| span_text(source, ann.type_annotation.span()));
        out.push(ServerParam { name, ty });
    }
    out
}

/// Verbatim, trimmed text of a span in `source`.
fn span_text(source: &str, span: oxc_span::Span) -> String {
    source[span.start as usize..span.end as usize].trim().to_string()
}

// ---------------------------------------------------------------------------
// Client call-site rewriting.
// ---------------------------------------------------------------------------

/// Replace identifier references to each extracted server fn in `client_source` with its client
/// binding expression.
///
/// The rewrite is identifier-aware: a name in `bindings` is only replaced when it appears as a whole
/// identifier (bounded by non-identifier chars) and outside strings/comments/template-literal text.
/// So `save(user)` becomes `client.save.post(user)` while `saved`, `mySave`, and the literal
/// `"save"` are left untouched.
pub fn rewrite_call_sites(client_source: &str, bindings: &HashMap<String, String>) -> String {
    if bindings.is_empty() {
        return client_source.to_string();
    }

    let bytes = client_source.as_bytes();
    let mut out = String::with_capacity(client_source.len());
    let scanner = TextScanner::new(client_source);
    let mut i = 0usize;

    while i < bytes.len() {
        // Pass through strings/comments/templates verbatim.
        if let Some(next) = scanner.skip_noncode(i) {
            out.push_str(&client_source[i..next]);
            i = next;
            continue;
        }

        // At an identifier start that is not preceded by an identifier char?
        if is_ident_start(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident_byte(bytes[j]) {
                j += 1;
            }
            let word = &client_source[i..j];
            // A member access (`foo.save`) is not a free reference to `save`; skip those.
            let is_member = i > 0 && bytes[i - 1] == b'.';
            if !is_member {
                if let Some(binding) = bindings.get(word) {
                    out.push_str(binding);
                    i = j;
                    continue;
                }
            }
            out.push_str(word);
            i = j;
            continue;
        }

        out.push(bytes[i] as char);
        i += 1;
    }

    out
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b == b'$' || b.is_ascii_alphabetic()
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_server_block_lifts_async_fn_and_removes_block() {
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) {\n\
    return db.users.insert(user);\n\
  }\n\
}\n\
const greeting = 'hi';\n";

        let extraction = extract_server_block(source);

        // The `server { … }` block is gone from the client source, surrounding code intact.
        assert!(
            !extraction.client_source.contains("server {"),
            "server block not removed; got: {}",
            extraction.client_source
        );
        assert!(
            !extraction.client_source.contains("db.users.insert"),
            "server body leaked into client; got: {}",
            extraction.client_source
        );
        assert!(
            extraction.client_source.contains("import { User } from './user';"),
            "leading import lost; got: {}",
            extraction.client_source
        );
        assert!(
            extraction.client_source.contains("const greeting = 'hi';"),
            "trailing const lost; got: {}",
            extraction.client_source
        );

        // Exactly one server fn: async `save`, one param `user: User`.
        assert_eq!(extraction.server_fns.len(), 1, "expected one server fn");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "save");
        assert!(f.is_async, "save should be async");
        assert_eq!(f.params.len(), 1, "save should take one param");
        assert_eq!(f.params[0].name, "user");
        assert_eq!(f.params[0].ty.as_deref(), Some("User"));
    }

    #[test]
    fn extract_server_block_absent_returns_source_unchanged() {
        let source = "const x = 1;\nfunction f() { return 2; }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.client_source, source);
        assert!(extraction.server_fns.is_empty());
    }

    #[test]
    fn extract_ignores_braces_in_strings_and_comments() {
        // A `}` inside a string and a comment must not close the block early.
        let source = "server {\n\
  function f() {\n\
    const s = \"a } b\"; // trailing } brace\n\
    return s;\n\
  }\n\
}\nconst after = 1;\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].name, "f");
        assert!(
            extraction.client_source.contains("const after = 1;"),
            "trailing code lost; got: {}",
            extraction.client_source
        );
        assert!(!extraction.client_source.contains("function f"));
    }

    #[test]
    fn does_not_match_server_as_substring() {
        let source = "const servery = { x: 1 };\nconst y = 2;\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.client_source, source);
        assert!(extraction.server_fns.is_empty());
    }

    #[test]
    fn rewrite_call_sites_is_identifier_aware() {
        let mut bindings = HashMap::new();
        bindings.insert("save".to_string(), "client.save.post".to_string());

        let client = "const r = save(user);\nconst saved = true;\nconst s = \"save it\";\nobj.save();\n";
        let out = rewrite_call_sites(client, &bindings);

        // Free reference replaced.
        assert!(out.contains("client.save.post(user)"), "call not rewritten; got: {out}");
        // Longer identifier untouched.
        assert!(out.contains("const saved = true;"), "saved clobbered; got: {out}");
        // String literal untouched.
        assert!(out.contains("\"save it\""), "string clobbered; got: {out}");
        // Member access untouched.
        assert!(out.contains("obj.save();"), "member access clobbered; got: {out}");
    }

    #[test]
    fn elysia_plugin_emits_routes_and_bindings() {
        let source = "server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n";
        let extraction = extract_server_block(source);
        let plugin = ElysiaEdenPlugin;
        let emit = plugin.emit(&extraction.server_fns);

        assert!(emit.server_module.contains("new Elysia()"), "no Elysia app; got: {}", emit.server_module);
        assert!(emit.server_module.contains(".post('/__server/save'"), "no save route; got: {}", emit.server_module);
        assert_eq!(
            emit.client_bindings.get("save").map(String::as_str),
            Some("client.__server.save.post")
        );
    }

    #[test]
    fn registry_default_is_elysia() {
        let registry = PluginRegistry::with_defaults();
        assert_eq!(registry.default_plugin().map(|p| p.name()), Some("elysia-eden"));
        assert!(registry.get("elysia-eden").is_some());
        assert!(registry.get("nope").is_none());
    }

    #[test]
    fn extract_marker_use_server_directive_lifts_and_strips() {
        let source = "function loadUser(id: number) {\n\
  'use server';\n\
  return db.users.find(id);\n\
}\nconst x = 1;\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "expected one marker fn");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "loadUser");
        assert!(
            !f.source.contains("use server"),
            "'use server' directive not stripped from lifted source; got: {}",
            f.source
        );
        assert!(
            !extraction.client_source.contains("function loadUser"),
            "marker fn not removed from client; got: {}",
            extraction.client_source
        );
        assert!(extraction.client_source.contains("const x = 1;"));
    }

    #[test]
    fn extract_marker_dollar_suffix_arrow_const_lifts() {
        let source = "const doThing$$ = async (n: number) => { return n + 1; };\nconst keep = 2;\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "expected one $$-suffixed fn");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "doThing$$");
        assert!(f.is_async, "arrow should be async");
        assert_eq!(f.params.len(), 1);
        assert!(
            !extraction.client_source.contains("doThing$$"),
            "marker fn not removed from client; got: {}",
            extraction.client_source
        );
        assert!(extraction.client_source.contains("const keep = 2;"));
    }

    #[test]
    fn extract_server_block_lifts_arrow_const() {
        let source = "server {\n\
  const save = async (user: User) => { return db.insert(user); };\n\
}\nconst c = 1;\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "arrow-const in block not lifted");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "save");
        assert!(f.is_async);
        assert!(extraction.client_source.contains("const c = 1;"));
        assert!(
            !extraction.client_source.contains("db.insert"),
            "server body leaked into client; got: {}",
            extraction.client_source
        );
    }
}
