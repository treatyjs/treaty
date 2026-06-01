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

/// The directive that marks a server fn as a WebSocket transport (analogous to `'use server'`).
const USE_WEBSOCKET_DIRECTIVE: &str = "use websocket";

mod axum_backend;
mod elysia;
mod express;
mod ts_to_rust;
pub use axum_backend::AxumBackendPlugin;
pub use elysia::ElysiaEdenPlugin;
pub use express::ExpressBackendPlugin;

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

/// How a server function is transported between client and server.
///
/// The kind is detected during extraction (see [`build_server_fn`]) and drives per-kind code
/// generation in each backend:
///   * [`TransportKind::Api`] — a plain request/response route (the historical default).
///   * [`TransportKind::Stream`] — a server-push stream (axum SSE / streaming body, elysia stream
///     handler). Detected when the fn is a generator (`function*` / `async function*`) or its body
///     yields.
///   * [`TransportKind::WebSocket`] — a bidirectional WebSocket upgrade. Detected when the fn carries
///     a leading `'use websocket'` string directive (analogous to `'use server'`) or its name follows
///     the `ws` convention (a `ws`-prefixed camelCase name such as `wsChat`, or a `ws_`-prefixed
///     snake_case name such as `ws_chat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportKind {
    /// Plain request/response (the default).
    #[default]
    Api,
    /// Bidirectional WebSocket upgrade.
    WebSocket,
    /// Server-push stream (SSE / streaming body).
    Stream,
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
    /// The target language for this fn, so the registry can dispatch by language. Sourced from the
    /// optional `server:IDENT { … }` block tag (e.g. `rust`, `ts`, `php`); defaults to `rust` for a
    /// bare `server { … }` block and for both top-level marker forms.
    pub lang: String,
    /// The transport kind for this fn (see [`TransportKind`]), detected during extraction. Defaults
    /// to [`TransportKind::Api`].
    pub transport: TransportKind,
    /// Whether the fn was declared with an `export` at module scope (an `export function f$$()`, an
    /// `export const g = …`, an `export default function h()`, or any top-level fn in a file-level
    /// `'use server'` / `'use websocket'` module). Such a fn is part of the module's PUBLIC surface: a
    /// consumer `import { f } from './x'`s it, so after lifting its body the client must RE-EXPORT the
    /// name as its client binding (an RPC stub) or the import resolves to `undefined`. A non-exported
    /// fn (an in-component `server { … }` block fn, a bare top-level marker fn that is only called
    /// in-module) is NOT re-exported — its call sites are rewritten in place instead. Defaults to
    /// `false` (the conservative, non-re-exported case).
    pub exported: bool,
    /// The VERBATIM declaration text exactly as it appears in the original source — including any
    /// leading `'use server'` / `'use websocket'` directive that [`ServerFn::source`] strips. This is
    /// the precise byte sequence the client source map's `sourcesContent` embeds (the map carries the
    /// original authoring file), so the privacy redaction must blank THIS text, not the stripped
    /// [`ServerFn::source`] (whose directive removal would otherwise leave the body unmatched and so
    /// unredacted in the map). Empty only for synthetic test fns; every extracted fn carries it.
    pub verbatim_source: String,
}

/// The default target language when no `server:IDENT` tag is given.
const DEFAULT_LANG: &str = "rust";

/// The result of [`extract_server_block`]: the client source with the `server { … }` block removed,
/// plus the functions that were declared inside it. When no block is present, `client_source` is the
/// input unchanged and `server_fns` is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerExtraction {
    pub client_source: String,
    pub server_fns: Vec<ServerFn>,
    /// Verbatim source text of every NON-fn top-level statement that was stripped from the client
    /// because the module is file-level `'use server'` (a top-level `const`/helper/etc. that may hold
    /// a secret). These are server-only and must be redacted from the client source map as well, so
    /// the map's `sourcesContent` never embeds the secret even though the client CODE no longer
    /// carries it. Empty for non-file-level extractions and for modules with no such statements.
    pub server_only_sources: Vec<String>,
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
/// the default [`AxumBackendPlugin`] followed by the opt-in reference [`ElysiaEdenPlugin`].
pub struct PluginRegistry {
    plugins: Vec<Box<dyn BackendPlugin>>,
}

impl PluginRegistry {
    /// An empty registry (no plugins, no default).
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    /// A registry preloaded with the default [`AxumBackendPlugin`] (so a developer who never touches
    /// Rust still gets a working axum service), plus the opt-in reference [`ElysiaEdenPlugin`] and
    /// the opt-in [`ExpressBackendPlugin`] (both selectable by name). The first plugin registered is
    /// the default, so the axum backend is it; express is registered last and selected by name
    /// `"express"`.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(AxumBackendPlugin));
        registry.register(Box::new(ElysiaEdenPlugin));
        registry.register(Box::new(ExpressBackendPlugin));
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
    extract_server_block_with_type(source, SourceType::default().with_typescript(true))
}

/// Like [`extract_server_block`], but parses the marker pre-pass with a JSX-capable [`SourceType`].
///
/// The marker pre-pass ([`extract_marker_fns_with_type`]) parses the module with OXC to find `'use server'` /
/// `'use websocket'` directives and `$$`-suffixed declarations. A `.tjsx` / `.tsx` source whose
/// component body is JSX FAILS to parse as plain TypeScript, so a top-level `$$`/`'use server'`/`'use
/// websocket'` marker in a JSX file would never be seen (the parse yields no usable body) and its
/// body would leak to the client. Routing the JSX front-ends through this entry with
/// [`SourceType::tsx`] lets the marker pre-pass see the real declarations, so the `$$` JSX server fn
/// is extracted exactly like the `.ts` path.
pub fn extract_server_block_jsx(source: &str) -> ServerExtraction {
    extract_server_block_with_type(source, SourceType::tsx())
}

/// Like [`extract_server_block`], but for a `.treaty` SFC, whose interleaved HTML/CSS regions are not
/// valid TypeScript. The explicit `server[:LANG] { … }` block lift (a comment/string-aware text scan)
/// already works on the raw SFC; the parse-based MARKER lift (`'use server'` / `$$` / `'use websocket'`)
/// would otherwise fail because the whole-file TS parse chokes on the markup. So this masks every
/// non-JavaScript region of the SFC to length-preserving whitespace, runs the marker parse over that
/// masked view (which now parses as TS and whose spans map 1:1 back into the real source), and slices /
/// strips against the real source — so a `'use server'` / `$$` server fn declared in a `.treaty` JS
/// chunk is extracted exactly like the `.ts` path, carrying its REAL body.
pub fn extract_server_block_treaty(source: &str, js_masked: &str) -> ServerExtraction {
    extract_server_block_impl(
        source,
        SourceType::default().with_typescript(true),
        Some(js_masked),
    )
}

/// Shared implementation of [`extract_server_block`] / [`extract_server_block_jsx`], parameterized on
/// the [`SourceType`] used by the marker pre-pass so a JSX source parses with JSX enabled.
fn extract_server_block_with_type(source: &str, source_type: SourceType) -> ServerExtraction {
    extract_server_block_impl(source, source_type, None)
}

/// Shared implementation behind [`extract_server_block_with_type`] and [`extract_server_block_treaty`].
///
/// `marker_detect` is an optional byte-offset-identical detection view for the parse-based marker lift
/// (see [`extract_marker_fns_detect`]); when `None`, the post-block client source is its own detection
/// source (the `.ts`/`.tsx` paths). The `.treaty` path supplies a masked view so the marker parse sees
/// only the JS regions. The masked view describes the WHOLE source; the marker step is run over the
/// slice of it corresponding to the post-block client source.
fn extract_server_block_impl(
    source: &str,
    source_type: SourceType,
    marker_detect: Option<&str>,
) -> ServerExtraction {
    // 1. Lift every explicit `server[:LANG] { … }` block first. Multiple blocks are supported; each
    //    keeps its own language tag (bare `server { … }` defaults to `rust`).
    //
    //    When `marker_detect` is supplied (the `.treaty` masked view), the SAME block spans are removed
    //    from it in lockstep with the real source so the two stay byte-offset-identical for the
    //    parse-based marker step below.
    let mut client_source = String::with_capacity(source.len());
    let mut client_detect = marker_detect.map(|_| String::with_capacity(source.len()));
    let mut server_fns: Vec<ServerFn> = Vec::new();
    let mut cursor = 0usize;
    while let Some(block) = find_server_block(&source[cursor..]) {
        let ServerBlock { block_start, body_start, body_end, block_end, lang } = block;
        // Translate the block-relative offsets back to absolute positions in `source`.
        let block_start = cursor + block_start;
        let body_start = cursor + body_start;
        let body_end = cursor + body_end;
        let block_end = cursor + block_end;

        // The function declarations live in the brace body (exclusive of the braces themselves).
        let body = &source[body_start..body_end];
        let lang = lang.unwrap_or_else(|| DEFAULT_LANG.to_string());
        server_fns.extend(parse_server_fns(body, &lang));

        // Carry forward the text that precedes this block; resume scanning after it.
        client_source.push_str(&source[cursor..block_start]);
        if let (Some(detect), Some(mask)) = (client_detect.as_mut(), marker_detect) {
            detect.push_str(&mask[cursor..block_start]);
        }
        cursor = block_end;
    }
    // Whatever remains after the last block (or the whole source if there were no blocks).
    client_source.push_str(&source[cursor..]);
    if let (Some(detect), Some(mask)) = (client_detect.as_mut(), marker_detect) {
        detect.push_str(&mask[cursor..]);
    }

    // 2. Lift the remaining top-level marker forms (`'use server'` directive, `name$$` suffix,
    //    module-level `'use websocket'`) from whatever client source survived step 1, removing their
    //    declarations as we go. The detection view is the masked client source for `.treaty`, else the
    //    client source itself.
    let detect = client_detect.as_deref().unwrap_or(&client_source);
    let (rewritten, marker_fns, server_only_sources) =
        extract_marker_fns_detect(&client_source, detect, source_type);
    client_source = rewritten;
    server_fns.extend(marker_fns);

    ServerExtraction { client_source, server_fns, server_only_sources }
}

/// Scan top-level declarations for the marker conventions that do not use an explicit `server { … }`
/// wrapper, lift them out, and return the source with their declarations removed.
///
/// A top-level declaration is a server fn when any of the following hold:
///   * a `function` / `const NAME = (…) => …` whose body's first statement is the bare
///     `'use server'` string directive (the directive statement is stripped from the lifted source);
///   * a `function` / `const NAME = (…) => …` whose **name** ends in two `$` characters; or
///   * the **module itself** carries a file-level `'use server'` directive (a `'use server'` string
///     statement at the very top of the program). In that case EVERY top-level function /
///     arrow-const declaration in the module — including `export`ed and `export default` ones, and
///     `async function*` async generators — is a server fn and is lifted out, and the file-level
///     directive statement is removed from the client source.
///
/// Exported forms are unwrapped: `export function f(){…}`, `export const g = () => …`,
/// `export default function h(){…}`, and `export async function* s(){…}` are all considered, so a
/// file-level `'use server'` module exporting its server fns is fully extracted.
///
/// The scan parses `detect_source` once with OXC and removes the matched declarations by byte span
/// (highest span first so earlier offsets stay valid). Only program-top-level declarations are
/// considered. Parses with the caller-supplied [`SourceType`] so a JSX (`.tsx`/`.tjsx`) source — whose
/// component body is not valid plain TypeScript — is parsed with JSX enabled and its top-level markers
/// are seen (the `.ts`/`.treaty` callers pass a plain-TypeScript [`SourceType`]).
///
/// Module-level `'use websocket'` is supported alongside the file-level `'use server'` lift: a
/// top-of-program `'use websocket'` string directive turns the WHOLE module into a server module whose
/// exported fns are lifted as [`TransportKind::WebSocket`] server fns (the duplex analogue of a
/// file-level `'use server'` module), the directive statement is stripped from the client, and each
/// lifted fn is rewritten to its WebSocket client binding by the active backend.
///
/// `detect_source` is parsed to FIND the markers; each lifted fn's body text is sliced from — and the
/// matched spans stripped out of — `text_source`. `detect_source` MUST be byte-offset-identical to
/// `text_source` (same length, every byte at the same index), differing only in which bytes are
/// blanked: a `.treaty` SFC's interleaved HTML/CSS regions are not valid TypeScript, so the whole-file
/// parse fails and a `'use server'` / `$$` marker in the JS region is missed. The `.treaty` front-end
/// passes a `detect_source` with every non-JS region replaced by length-preserving whitespace (so the
/// parse sees only the JS, succeeds, and every span maps 1:1 back into the real `text_source`), while
/// `text_source` is the real client source whose JS bodies (and HTML) are intact — so the lifted fn
/// carries its REAL body and the strip lands on the real declaration. The `.ts`/`.tsx` callers pass the
/// same string for both (no masking needed).
fn extract_marker_fns_detect(
    text_source: &str,
    detect_source: &str,
    source_type: SourceType,
) -> (String, Vec<ServerFn>, Vec<String>) {
    debug_assert_eq!(
        text_source.len(),
        detect_source.len(),
        "detect_source must be byte-offset-identical to text_source"
    );
    if detect_source.trim().is_empty() {
        return (text_source.to_string(), Vec::new(), Vec::new());
    }
    // Slice fn bodies and strip spans from the REAL text source; parse the (possibly masked) detect
    // source so spans are found even when the real source interleaves non-TS regions.
    let source = text_source;

    let allocator = Allocator::default();
    let ret = JsParser::new(&allocator, detect_source, source_type).parse();

    // A file-level `'use server'` directive turns the WHOLE module into a server module: every
    // top-level function/arrow-const declaration is server-only, regardless of per-fn marker. A
    // file-level `'use websocket'` directive does the same, but classifies every lifted fn as a
    // WebSocket-transport server fn (the duplex-channel analogue). Either directive triggers the
    // whole-module lift; the websocket flavour additionally forces `TransportKind::WebSocket`.
    let file_level_server = ret
        .program
        .directives
        .iter()
        .any(|d| d.expression.value.as_str() == USE_SERVER_DIRECTIVE);
    let file_level_websocket = ret
        .program
        .directives
        .iter()
        .any(|d| d.expression.value.as_str() == USE_WEBSOCKET_DIRECTIVE);
    let file_level = file_level_server || file_level_websocket;

    let mut fns = Vec::new();
    // Byte spans of the top-level declarations we remove from the client source.
    let mut removals: Vec<(usize, usize)> = Vec::new();
    // Verbatim source of stripped NON-fn server-only top-level statements (file-level modules only),
    // so the client source map can redact them too.
    let mut server_only_sources: Vec<String> = Vec::new();

    // When the module is file-level `'use server'` / `'use websocket'`, strip every leading string
    // directive statement (e.g. `'use server'` / `'use websocket'`) from the client source so the
    // lifted module marker does not survive into the client bundle.
    if file_level {
        for d in &ret.program.directives {
            let v = d.expression.value.as_str();
            if v == USE_SERVER_DIRECTIVE || v == USE_WEBSOCKET_DIRECTIVE {
                removals.push((d.span.start as usize, d.span.end as usize));
            }
        }
    }

    for stmt in &ret.program.body {
        // Inside a file-level `'use server'`/`'use websocket'` module no per-fn marker is required;
        // otherwise the declaration must carry its own marker (`'use server'` body directive or `$$`
        // suffix).
        if let Some((mut server_fn, span)) = server_fn_from_top_level(source, stmt, !file_level) {
            // A file-level `'use websocket'` module forces every lifted fn onto the WebSocket
            // transport (the duplex-channel analogue of a file-level `'use server'` module).
            if file_level_websocket {
                server_fn.transport = TransportKind::WebSocket;
            }
            // A file-level `'use server'` / `'use websocket'` module is wholly a server module: every
            // lifted fn (even a bare, non-`export` declaration) is the module's public surface a
            // consumer imports, so it must be re-exported as its client binding.
            if file_level {
                server_fn.exported = true;
            }
            removals.push(span);
            fns.push(server_fn);
            continue;
        }

        // A file-level `'use server'` module is server-only IN FULL: every top-level RUNTIME
        // statement that is not a server fn — a `const`/`let`/`var` (which may hold a secret such as
        // a DB key, like `const DB_API_KEY = …`), a non-exported helper `function`/`class`, or a bare
        // expression statement — is also stripped from the client source. Without this, a top-level
        // secret declared beside the server fns would survive into the client bundle even though the
        // fn BODIES were lifted. Imports and pure TYPE declarations (`interface`/`type`, which carry
        // no runtime value and are erased by the bundler) are kept so a consumer can still import the
        // module's types.
        if file_level && is_server_only_runtime_statement(stmt) {
            let (start, end) = (stmt.span().start as usize, stmt.span().end as usize);
            removals.push((start, end));
            server_only_sources.push(source[start..end].to_string());
        }
    }

    let client_source = strip_spans(source, &mut removals);
    (client_source, fns, server_only_sources)
}

/// Whether a top-level statement of a file-level `'use server'` module is RUNTIME code that must be
/// stripped from the client source (it is server-only). True for variable declarations, function and
/// class declarations, bare expression statements, and `export`s wrapping any of those. False for
/// `import` declarations and pure TYPE declarations (`interface` / `type` alias, and `export`s of
/// them), which carry no runtime value and are kept so the module's types stay importable.
fn is_server_only_runtime_statement(stmt: &Statement) -> bool {
    use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind};
    match stmt {
        // Imports and type-only declarations are not runtime-bearing: keep them.
        Statement::ImportDeclaration(_)
        | Statement::TSInterfaceDeclaration(_)
        | Statement::TSTypeAliasDeclaration(_)
        | Statement::TSEnumDeclaration(_)
        | Statement::TSModuleDeclaration(_)
        | Statement::TSImportEqualsDeclaration(_) => false,
        // Runtime declarations are server-only.
        Statement::VariableDeclaration(_)
        | Statement::FunctionDeclaration(_)
        | Statement::ClassDeclaration(_)
        | Statement::ExpressionStatement(_) => true,
        // An `export …` wrapping a runtime declaration is server-only; an `export interface`/
        // `export type` is a pure type re-export and is kept. (A `TSTypeAliasDeclaration`/
        // `TSInterfaceDeclaration` inside an `ExportNamedDeclaration` is type-only.)
        Statement::ExportNamedDeclaration(export) => match export.declaration.as_ref() {
            None => false, // `export { … }` / `export … from …`: a re-export, keep it.
            Some(Declaration::TSInterfaceDeclaration(_))
            | Some(Declaration::TSTypeAliasDeclaration(_))
            | Some(Declaration::TSEnumDeclaration(_))
            | Some(Declaration::TSModuleDeclaration(_)) => false,
            Some(_) => true,
        },
        Statement::ExportDefaultDeclaration(export) => !matches!(
            &export.declaration,
            ExportDefaultDeclarationKind::TSInterfaceDeclaration(_)
        ),
        _ => false,
    }
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

/// A located top-level `server[:LANG] { … }` block. `block_start..block_end` is the full block
/// (`server` keyword through the closing `}`, including a trailing newline if present) and
/// `body_start..body_end` is the brace *interior*. `lang` is the optional `:IDENT` tag text.
struct ServerBlock {
    block_start: usize,
    body_start: usize,
    body_end: usize,
    block_end: usize,
    lang: Option<String>,
}

/// Locate a top-level `server[:LANG] { … }` block, optionally tagged with a language as in
/// `server:ts { … }`. The bare `server { … }` form yields `lang = None` (the caller defaults it to
/// `rust`).
fn find_server_block(source: &str) -> Option<ServerBlock> {
    let bytes = source.as_bytes();
    let mut i = 0usize;

    // A `server` keyword only opens a block when it is a standalone identifier (not e.g. `servery`
    // or `x.server`), outside any string/comment. Blocks are matched at *any* brace depth so that a
    // `server { … }` declared inside a component function body (depth > 0) is lifted exactly like a
    // top-level one; a leading-token guard (see `server_in_statement_position`) keeps an object
    // property `server: { … }` or a member access `x.server` from being mistaken for a block.
    let mut scanner = TextScanner::new(source);
    while i < bytes.len() {
        // Advance the scanner state up to `i`, skipping strings/comments wholesale.
        if let Some(next) = scanner.skip_noncode(i) {
            i = next;
            continue;
        }

        if matches_keyword(source, i, "server") && server_in_statement_position(source, i) {
            // After `server`, allow whitespace, an optional `:IDENT` language tag, more whitespace,
            // then require a `{`.
            let after_kw = i + "server".len();
            let mut j = after_kw;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }

            let mut lang = None;
            if j < bytes.len() && bytes[j] == b':' {
                // Parse the language identifier after the colon.
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                let ident_start = k;
                while k < bytes.len() && is_ident_byte(bytes[k]) {
                    k += 1;
                }
                if k > ident_start {
                    lang = Some(source[ident_start..k].to_string());
                    j = k;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                }
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
                return Some(ServerBlock {
                    block_start: i,
                    body_start,
                    body_end,
                    block_end,
                    lang,
                });
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
    // Byte-level prefix match so this is safe to call at any index, even one that falls inside a
    // multibyte UTF-8 char (the scanner walks byte-by-byte across non-ASCII text).
    if bytes.len() < i + keyword.len() || &bytes[i..i + keyword.len()] != keyword.as_bytes() {
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

/// Is the `server` keyword at byte `i` in statement position, i.e. a place a `server { … }` block
/// may legally begin?
///
/// Because blocks are matched at any brace depth (so in-component blocks lift like top-level ones),
/// this guards against the non-block uses that `matches_keyword` alone would accept:
///   * a member access `x.server { … }` (the preceding code token is `.`), and
///   * an object-literal property `{ server: … }` (the `:`-then-value shape, also rejected by the
///     caller, which requires a `{` directly after the optional `:LANG` tag).
///
/// A `server` block legitimately begins at the start of input, after a statement terminator (`;`), or
/// after a block boundary (`{` / `}`). It may ALSO begin after a *preceding statement that omitted its
/// semicolon* (the TS-by-default authoring style the `.treaty` spec encourages) — in that case
/// JavaScript's automatic-semicolon-insertion (ASI) treats the line break before `server` as the
/// statement boundary. So a token that can legally *end* a statement (an identifier/keyword char, a
/// string/template close quote, a numeric literal, or a `)` / `]`) followed by a newline before
/// `server` is also statement position.
///
/// The scan is comment- and string-aware: it walks the source from the start up to `i`, skipping
/// strings/templates/comments wholesale (via [`TextScanner`]), so a `//`-comment ending in `.` or an
/// import string ending in `'…'` on the line above `server {` no longer fools the guard. It records
/// the last meaningful code byte before `i` and whether a newline separated that byte from `server`.
fn server_in_statement_position(source: &str, i: usize) -> bool {
    let bytes = source.as_bytes();

    // The last ordinary-code byte seen before `i`, and whether a line break has occurred since it.
    let mut last_code: Option<u8> = None;
    let mut newline_since_last_code = false;

    let scanner = TextScanner::new(source);
    let mut p = 0usize;
    while p < i {
        // Skip a string/template/comment wholesale. A comment counts as "whitespace" for ASI: any
        // newline inside or after it still separates the preceding code token from `server`.
        if let Some(next) = scanner.skip_noncode(p) {
            // Clamp to `i` so we never read past the keyword we are classifying.
            let end = next.min(i);
            if source.as_bytes()[p..end].contains(&b'\n') {
                newline_since_last_code = true;
            }
            p = next;
            continue;
        }

        let b = bytes[p];
        if b.is_ascii_whitespace() {
            if b == b'\n' {
                newline_since_last_code = true;
            }
        } else {
            last_code = Some(b);
            newline_since_last_code = false;
        }
        p += 1;
    }

    match last_code {
        // Start of input, or the previous token explicitly ended a statement/opened a block.
        None => true,
        Some(b'{') | Some(b'}') | Some(b';') => true,
        // ASI: a token that can end a statement, followed by a line break, opens a new statement.
        Some(b) if newline_since_last_code && can_end_statement(b) => true,
        _ => false,
    }
}

/// Can the byte `b` be the final character of a JavaScript expression/statement, such that a line
/// break after it triggers automatic-semicolon insertion?
///
/// True for identifier/keyword characters (e.g. the `e` of `types` or a bare `null`), a closing
/// string/template quote (`'` `"` `` ` ``), and the closing `)` / `]` of a call/index/group. These
/// are exactly the token-enders that precede a no-semicolon `server { … }` block in TS-by-default
/// authoring. A `,`, `.`, `=`, `(`, `[`, `:` etc. cannot end a statement, so `server` after one of
/// those (even across a newline) is a value, not a block.
fn can_end_statement(b: u8) -> bool {
    is_ident_byte(b) || matches!(b, b'\'' | b'"' | b'`' | b')' | b']')
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
fn parse_server_fns(body: &str, lang: &str) -> Vec<ServerFn> {
    let mut fns = Vec::new();
    if body.trim().is_empty() {
        return fns;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, body, source_type).parse();

    for stmt in &ret.program.body {
        // Inside an explicit `server { … }` block, every declaration is server-only, so we do not
        // gate on a marker — `require_marker = false`. Each lifted fn inherits the block's `lang`.
        if let Some((server_fn, _span)) = build_server_fn(body, stmt, false, lang) {
            fns.push(server_fn);
        }
    }

    fns
}

/// Decide whether a program-top-level statement is a server fn and, if so, build it.
///
/// Unwraps `export` / `export default` declarations so an exported `function` / arrow-const /
/// `async function*` is considered the same as a bare one. `require_marker` is threaded to
/// [`build_server_fn`]: it is `true` for the per-declaration marker conventions (`'use server'` body
/// directive or `$$`-suffixed name) and `false` when the enclosing module is file-level
/// `'use server'` (then every top-level fn is server-only).
///
/// Returns the [`ServerFn`] plus the byte span of the WHOLE top-level statement (the `export`
/// keyword included, when present) so the caller removes the entire declaration from the client
/// source rather than leaving a dangling `export`.
fn server_fn_from_top_level(
    source: &str,
    stmt: &Statement,
    require_marker: bool,
) -> Option<(ServerFn, (usize, usize))> {
    use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind};
    // Top-level marker forms always target `rust` (the default backend language).
    match stmt {
        Statement::ExportNamedDeclaration(export) => {
            let (mut server_fn, _inner_span) = match export.declaration.as_ref()? {
                Declaration::FunctionDeclaration(func) => {
                    build_from_function(source, func, require_marker, DEFAULT_LANG)
                }
                Declaration::VariableDeclaration(decl) => {
                    build_from_var_decl(source, decl, require_marker, DEFAULT_LANG)
                }
                _ => None,
            }?;
            // An `export`ed declaration is part of the module's public surface, so a consumer imports
            // it by name — the client must re-export its binding (see [`ServerFn::exported`]).
            server_fn.exported = true;
            // Remove the whole `export …` statement (so no dangling `export` remains); the function
            // source the backend re-emits never includes the `export` keyword.
            Some((server_fn, (export.span.start as usize, export.span.end as usize)))
        }
        Statement::ExportDefaultDeclaration(export) => {
            let ExportDefaultDeclarationKind::FunctionDeclaration(func) = &export.declaration else {
                return None;
            };
            let (mut server_fn, _inner_span) =
                build_from_function(source, func, require_marker, DEFAULT_LANG)?;
            server_fn.exported = true;
            Some((server_fn, (export.span.start as usize, export.span.end as usize)))
        }
        _ => build_server_fn(source, stmt, require_marker, DEFAULT_LANG),
    }
}

/// Build a [`ServerFn`] from a `function` declaration or a single-declarator `const NAME = (…) => …`
/// arrow-const statement. Handles both forms uniformly.
///
/// When `require_marker` is true the statement is only treated as a server fn if it carries one of
/// the markers: a `$$`-suffixed name, or a `'use server'` directive as the first body statement
/// (which is then stripped from the emitted source). When false (inside a `server { … }` block or a
/// file-level `'use server'` module) the declaration is always lifted and any leading `'use server'`
/// directive is still stripped.
///
/// Returns the built fn and the byte span `(start, end)` of the full statement in `source`.
fn build_server_fn(
    source: &str,
    stmt: &Statement,
    require_marker: bool,
    lang: &str,
) -> Option<(ServerFn, (usize, usize))> {
    match stmt {
        Statement::FunctionDeclaration(func) => {
            build_from_function(source, func, require_marker, lang)
        }
        Statement::VariableDeclaration(decl) => {
            build_from_var_decl(source, decl, require_marker, lang)
        }
        _ => None,
    }
}

/// Build a [`ServerFn`] from a `function` declaration node (including `async function*` generators).
/// Shared by the bare-statement, `server { … }`-block, and `export`-wrapped paths.
fn build_from_function(
    source: &str,
    func: &oxc_ast::ast::Function,
    require_marker: bool,
    lang: &str,
) -> Option<(ServerFn, (usize, usize))> {
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

    let has_ws_directive = func
        .body
        .as_deref()
        .is_some_and(|body| has_leading_directive(body, USE_WEBSOCKET_DIRECTIVE));
    let body_src = func
        .body
        .as_deref()
        .map(|b| span_text(source, b.span))
        .unwrap_or_default();
    let transport = detect_transport(&name, func.generator, has_ws_directive, &body_src);

    let span = (func.span.start as usize, func.span.end as usize);
    // The verbatim declaration text as it appears in the original source (directive included) — what
    // the client map's `sourcesContent` embeds, so the privacy redaction can blank it exactly.
    let verbatim_source = source[span.0..span.1].to_string();
    // Strip the `'use server'` directive (by span) then the `'use websocket'` directive (by
    // content) from the lifted source, so neither marker survives into the emitted body.
    let src = strip_directive(source, span, directive);
    let src = strip_leading_directive_text(&src, USE_WEBSOCKET_DIRECTIVE);

    let params = collect_params(source, &func.params);
    let return_type = func
        .return_type
        .as_ref()
        .map(|ann| span_text(source, ann.type_annotation.span()));

    Some((
        ServerFn {
            name,
            source: src,
            params,
            return_type,
            is_async: func.r#async,
            lang: lang.to_string(),
            transport,
            // Set by the caller that knows the declaration's export context (the `export`-wrapped path
            // in `server_fn_from_top_level`, or a file-level module). Defaults to non-exported here.
            exported: false,
            verbatim_source,
        },
        span,
    ))
}

/// Build a [`ServerFn`] from a single-declarator `const NAME = (…) => …` arrow-const variable
/// declaration. Shared by the bare-statement, `server { … }`-block, and `export`-wrapped paths.
fn build_from_var_decl(
    source: &str,
    decl: &oxc_ast::ast::VariableDeclaration,
    require_marker: bool,
    lang: &str,
) -> Option<(ServerFn, (usize, usize))> {
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

    let has_ws_directive = has_leading_directive(&arrow.body, USE_WEBSOCKET_DIRECTIVE);
    let body_src = span_text(source, arrow.body.span);
    // Arrow functions cannot be generators (`function*`), so generator detection rests on the
    // body yielding (an arrow wrapping a generator body) plus the name/directive markers.
    let transport = detect_transport(&name, false, has_ws_directive, &body_src);

    let span = (decl.span.start as usize, decl.span.end as usize);
    // Verbatim declaration text (directive included) — what the client map embeds; see
    // [`ServerFn::verbatim_source`].
    let verbatim_source = source[span.0..span.1].to_string();
    let src = strip_directive(source, span, directive);
    let src = strip_leading_directive_text(&src, USE_WEBSOCKET_DIRECTIVE);

    let params = collect_params(source, &arrow.params);
    let return_type = arrow
        .return_type
        .as_ref()
        .map(|ann| span_text(source, ann.type_annotation.span()));

    Some((
        ServerFn {
            name,
            source: src,
            params,
            return_type,
            is_async: arrow.r#async,
            lang: lang.to_string(),
            transport,
            // Set by the caller that knows the declaration's export context. Defaults to non-exported.
            exported: false,
            verbatim_source,
        },
        span,
    ))
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

/// Does the function/arrow body begin with the bare `directive` string directive (e.g.
/// `'use websocket'`)? OXC parses leading string-literal statements as `FunctionBody::directives`.
fn has_leading_directive(body: &oxc_ast::ast::FunctionBody, directive: &str) -> bool {
    body.directives
        .iter()
        .any(|d| d.expression.value.as_str() == directive)
}

/// Detect the [`TransportKind`] for a server fn from its name, generator flag, an explicit WebSocket
/// directive, and its body text.
///
/// Precedence: an explicit WebSocket marker (a `'use websocket'` directive or a `ws` name convention)
/// wins, then a streaming shape (a `function*` / `async function*` generator, or a body that
/// `yield`s), otherwise [`TransportKind::Api`].
fn detect_transport(name: &str, is_generator: bool, has_ws_directive: bool, body_src: &str) -> TransportKind {
    if has_ws_directive || is_ws_name(name) {
        TransportKind::WebSocket
    } else if is_generator || body_yields(body_src) {
        TransportKind::Stream
    } else {
        TransportKind::Api
    }
}

/// Is `name` a WebSocket-convention name? Either a `ws`-prefixed camelCase name (`ws` followed by an
/// uppercase letter, e.g. `wsChat`) or a `ws_`-prefixed snake_case name (e.g. `ws_chat`).
fn is_ws_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("ws_") {
        return !rest.is_empty();
    }
    if let Some(rest) = name.strip_prefix("ws") {
        return rest.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    }
    false
}

/// Does `body_src` contain a `yield` keyword as a whole identifier outside strings/comments/templates?
/// Used to classify an arrow/function body that yields as a [`TransportKind::Stream`] even when the
/// generator star is not directly visible on the lifted node.
fn body_yields(body_src: &str) -> bool {
    let bytes = body_src.as_bytes();
    let scanner = TextScanner::new(body_src);
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(next) = scanner.skip_noncode(i) {
            i = next;
            continue;
        }
        if is_ident_start(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident_byte(bytes[j]) {
                j += 1;
            }
            if &body_src[i..j] == "yield" {
                return true;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    false
}

/// Remove a leading `'<directive>'` string-directive statement from already-sliced function source.
///
/// Operates on the lifted declaration text (not the original `source`), scanning for the first
/// `{`-delimited body and, if its first non-whitespace statement is the quoted `directive`, excising
/// that statement plus a trailing `;`, surrounding whitespace, and one newline. A no-op when the
/// directive is absent.
fn strip_leading_directive_text(src: &str, directive: &str) -> String {
    let Some(open) = src.find('{') else {
        return src.to_string();
    };
    let bytes = src.as_bytes();
    let mut i = open + 1;
    // Skip whitespace to the first statement.
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let stmt_start = i;
    // The directive must appear as a single- or double-quoted string literal equal to `directive`.
    for quote in [b'\'', b'"'] {
        let lit = format!("{q}{directive}{q}", q = quote as char);
        if src[stmt_start..].starts_with(&lit) {
            let mut rel_end = stmt_start + lit.len();
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
            let mut start = stmt_start;
            while start > 0 && (bytes[start - 1] == b' ' || bytes[start - 1] == b'\t') {
                start -= 1;
            }
            let mut out = src.to_string();
            out.replace_range(start..rel_end, "");
            return out;
        }
    }
    src.to_string()
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

        // An ordinary byte that is neither noncode nor an identifier start. It may be the LEAD byte of
        // a multibyte UTF-8 char (e.g. `ɵ`, which is what the lowered Ivy emit on the `@Component`
        // server path is full of). Copy the WHOLE char — slicing through `client_source` to the next
        // char boundary — rather than `bytes[i] as char`, which would split the char into its raw bytes
        // and corrupt it (the `ɵɵdefineComponent` mojibake bug).
        let char_len = utf8_char_len(bytes[i]);
        let end = (i + char_len).min(bytes.len());
        out.push_str(&client_source[i..end]);
        i = end;
    }

    out
}

/// The byte length of the UTF-8 char whose lead byte is `b` (1 for ASCII / a continuation byte, up to
/// 4 for a 4-byte sequence). Used to copy a whole multibyte char in one step.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 || (0x80..0xC0).contains(&b) {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b == b'$' || b.is_ascii_alphabetic()
}

// ---------------------------------------------------------------------------
// Client resource-binding runtime imports.
// ---------------------------------------------------------------------------

/// The npm module the emitted client bindings import their resource helper from. This is the real,
/// published `@treaty/httpclient` resources entry point (see `libs/treaty/edenclient`'s `package.json`
/// `exports["./resources"]`), whose surface includes [`RESOURCE_HELPER`].
pub const RESOURCE_CLIENT_MODULE: &str = "@treaty/httpclient/resources";

/// The single resource helper the axum backend's request/response client bindings reference.
/// `@treaty/httpclient` exports `edenResource` / `edenHttpResource` (observable, `rxResource`-backed)
/// and `edenPromiseResource` (promise-backed, `resource()`-backed). The Api binding is a one-shot
/// promise factory (`fetch(...).then(...)`), so it wraps the REAL promise-backed `edenPromiseResource`.
///
/// The Stream binding is NOT a one-shot resource: a stream-transport server fn is an async generator,
/// so its client binding is a native async-iterable factory backed by `EventSource` (consumed with
/// `for await`) — it needs no runtime helper and therefore does not reference this symbol. The
/// WebSocket binding likewise opens a live `WebSocket`. Only the request/response (Api) transport pulls
/// this import in (the emitter is keyed on the helper appearing in the emitted code).
pub const RESOURCE_HELPER: &str = "edenPromiseResource";

/// The runtime symbol the emitted client bindings reference, by transport. Every transport's binding
/// (see `axum_backend`) wraps the REAL [`RESOURCE_HELPER`] export, so a lifted server fn is what
/// pulls the import in. Kept for transport-driven callers; the code-keyed
/// [`client_runtime_imports_for_code`] is what the front-ends use.
pub fn client_runtime_imports(fns: &[ServerFn]) -> String {
    // Only the request/response (Api) binding wraps the promise-backed [`RESOURCE_HELPER`]; the Stream
    // binding is a native async-iterable factory and the WebSocket binding opens a live socket, neither
    // of which references the helper, so neither pulls the import in.
    let needs_resource = fns
        .iter()
        .any(|f| matches!(f.transport, TransportKind::Api));
    client_runtime_imports_for(needs_resource)
}

/// Emit the REAL `import` of the resource-client helper the emitted client `code` references, so every
/// binding identifier the lifted server fns were rewritten to resolves at boot from the published
/// `@treaty/httpclient` package rather than throwing `<symbol> is not defined` (the log-viewer boot
/// crash) — and WITHOUT defining a self-contained stub for it.
///
/// Keyed off the emitted code (not the transport list) so a non-axum backend, whose distinct binding
/// shape does not name [`RESOURCE_HELPER`] at all, gets no spurious import: the import is emitted only
/// when the code genuinely references the helper.
pub fn client_runtime_imports_for_code(code: &str) -> String {
    client_runtime_imports_for(code.contains(RESOURCE_HELPER))
}

/// Shared import builder for the transport-keyed and code-keyed selectors above.
fn client_runtime_imports_for(needs_resource: bool) -> String {
    if !needs_resource {
        return String::new();
    }
    // A REAL import of the published resource helper — not a generated shim. `fetch` / `EventSource` /
    // `WebSocket` the bindings also name are browser globals (no import).
    format!(
        "// Treaty server-fn client runtime: import the resource helper the lifted server fns are\n\
         // rewritten to so every binding resolves at boot (no `{helper}` left undefined).\n\
         import {{ {helper} }} from '{module}';\n",
        helper = RESOURCE_HELPER,
        module = RESOURCE_CLIENT_MODULE,
    )
}

// ---------------------------------------------------------------------------
// Shared server-fn binding re-export.
// ---------------------------------------------------------------------------

/// Re-export every lifted server fn as its client binding at module scope, for any lifted fn whose
/// NAME does not already appear as a free reference rewritten in `code`.
///
/// A lifted server fn reaches the client in one of two shapes:
///   * it was CALLED somewhere in the client (a template handler / free reference): [`rewrite_call_sites`]
///     (and, on the `@Component` path, the `ctx.<fn>(` swap) already replaced the call with the binding
///     expression, so nothing more is needed; OR
///   * it was a SIBLING module export the author imports elsewhere (`import { loadUser$$ } from './x'`):
///     the lift removed its declaration, so a consumer would now import `undefined`. For these the
///     binding must be re-exported under the same name so the consumer transparently receives the RPC
///     stub instead of the (lifted) body.
///
/// This appends `export const <name> = <binding>;` for every fn marked [`ServerFn::exported`] — a fn
/// that was an `export` declaration (or any fn in a file-level `'use server'`/`'use websocket'` module),
/// i.e. part of the module's PUBLIC surface a consumer imports. A NON-exported fn (an in-component
/// `server { … }` block fn, a bare top-level marker fn called only in-module) is NOT re-exported: its
/// call sites were already rewritten in place, and re-exporting it would emit a duplicate binding under
/// a name no external consumer imports. The binding is the active backend's per-fn client expression (a
/// `fetch`/`EventSource`/`WebSocket` factory) — the body never appears. Front-ends call this so EVERY
/// authoring form (`@Component` `.ts`, plain `.ts`, `.treaty`, JSX) emits the client binding for a
/// lifted exported server fn, not just the in-component call-site rewrite.
pub fn export_server_fn_bindings(
    code: &str,
    fns: &[ServerFn],
    bindings: &HashMap<String, String>,
) -> String {
    let mut out = code.to_string();
    for f in fns {
        if !f.exported {
            continue;
        }
        let Some(binding) = bindings.get(&f.name) else {
            continue;
        };
        out.push_str(&format!("\nexport const {} = {};\n", f.name, binding));
    }
    out
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal [`ServerFn`] for the re-export unit tests, parameterized on `exported`.
    fn test_fn(name: &str, exported: bool) -> ServerFn {
        ServerFn {
            name: name.to_string(),
            source: String::new(),
            params: Vec::new(),
            return_type: None,
            is_async: true,
            lang: "rust".to_string(),
            transport: TransportKind::Api,
            exported,
            verbatim_source: String::new(),
        }
    }

    #[test]
    fn rewrite_call_sites_preserves_multibyte_chars() {
        // REGRESSION: a multibyte UTF-8 char (the Ivy `ɵ`, U+0275 = bytes 0xC9 0xB5) in the client
        // source must survive `rewrite_call_sites` intact. The byte-at-a-time `bytes[i] as char` path
        // split it into two Latin-1 chars (the `ɵɵdefineComponent` mojibake) — copying the whole char
        // fixes it. This matters because the `@Component` server path rewrites call sites over the
        // ALREADY-LOWERED Ivy emit, which is full of `ɵ`.
        let mut bindings = HashMap::new();
        bindings.insert("save".to_string(), "client.save".to_string());
        let client = "i0.\u{0275}\u{0275}defineComponent({ x: save() });\n";
        let out = rewrite_call_sites(client, &bindings);
        assert!(
            out.contains("\u{0275}\u{0275}defineComponent"),
            "multibyte ɵ corrupted by rewrite; got bytes: {:?}",
            out.chars().map(|c| c as u32).collect::<Vec<_>>()
        );
        assert!(out.contains("client.save()"), "call not rewritten; got: {out}");
        // The output must be valid UTF-8 round-tripping the original ɵ codepoints (no 0xC9/0xB5 split).
        assert!(!out.chars().any(|c| c as u32 == 0x00C9), "ɵ was split into Latin-1 bytes; got: {out}");
    }

    #[test]
    fn export_server_fn_bindings_reexports_exported_fn() {
        let mut bindings = HashMap::new();
        bindings.insert("loadUser$$".to_string(), "((id) => fetchThing(id))".to_string());
        let fns = vec![test_fn("loadUser$$", true)];
        // An EXPORTED lifted fn is part of the module surface, so it is re-exported under its name.
        let out = export_server_fn_bindings("const x = 1;", &fns, &bindings);
        assert!(
            out.contains("export const loadUser$$ = ((id) => fetchThing(id));"),
            "no re-export for an exported lifted fn; got:\n{out}"
        );
    }

    #[test]
    fn export_server_fn_bindings_skips_non_exported_fn() {
        let mut bindings = HashMap::new();
        bindings.insert("save".to_string(), "client.save".to_string());
        let fns = vec![test_fn("save", false)];
        // A NON-exported fn (an in-component `server { … }` block fn whose call site was rewritten in
        // place) is NOT re-exported — no consumer imports it, and a re-export would duplicate-bind it.
        let out = export_server_fn_bindings("const r = save();", &fns, &bindings);
        assert!(
            !out.contains("export const save ="),
            "re-exported a non-exported (in-component) fn; got:\n{out}"
        );
    }

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
    fn registry_default_is_axum() {
        let registry = PluginRegistry::with_defaults();
        // The axum backend is the default; elysia-eden stays registered but is opt-in by name.
        assert_eq!(registry.default_plugin().map(|p| p.name()), Some("axum"));
        assert!(registry.get("axum").is_some());
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

    #[test]
    fn bare_server_block_defaults_lang_to_rust() {
        let source = "server {\n\
  function f() { return 1; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].lang, "rust");
    }

    #[test]
    fn tagged_server_block_sets_ts_lang() {
        let source = "server:ts {\n\
  function f() { return 1; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].name, "f");
        assert_eq!(extraction.server_fns[0].lang, "ts");
    }

    #[test]
    fn tagged_server_block_sets_php_lang() {
        let source = "server:php {\n\
  function f() { return 1; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].lang, "php");
    }

    #[test]
    fn dollar_marker_fn_defaults_lang_to_rust() {
        let source = "const doThing$$ = async (n: number) => { return n + 1; };\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].lang, "rust");
    }

    #[test]
    fn use_server_marker_fn_defaults_lang_to_rust() {
        let source = "function loadUser(id: number) {\n\
  'use server';\n\
  return db.users.find(id);\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].lang, "rust");
    }

    #[test]
    fn plain_server_fn_is_classified_api() {
        let source = "server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Api);
    }

    #[test]
    fn async_generator_server_fn_is_classified_stream() {
        // An `async function*` generator inside a server block is a streaming transport.
        let source = "server {\n\
  async function* ticks() { yield 1; yield 2; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1, "expected one fn");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "ticks");
        assert_eq!(f.transport, TransportKind::Stream, "generator should be Stream");
    }

    #[test]
    fn yielding_body_is_classified_stream() {
        // A non-`*` declaration whose body yields is still treated as a stream.
        let source = "server {\n\
  function feed() { yield 1; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Stream);
    }

    #[test]
    fn use_websocket_directive_is_classified_websocket_and_stripped() {
        let source = "server {\n\
  function chat(msg: string) { 'use websocket'; return msg; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        let f = &extraction.server_fns[0];
        assert_eq!(f.transport, TransportKind::WebSocket, "ws directive should classify WebSocket");
        assert!(
            !f.source.contains("use websocket"),
            "'use websocket' directive not stripped from lifted source; got: {}",
            f.source
        );
    }

    #[test]
    fn ws_name_convention_is_classified_websocket() {
        // A `ws`-prefixed camelCase name and a `ws_` snake_case name are both WebSocket.
        let source = "server {\n\
  function wsChat(msg: string) { return msg; }\n\
  function ws_feed(n: number) { return n; }\n\
}\n";
        let extraction = extract_server_block(source);
        let chat = extraction.server_fns.iter().find(|f| f.name == "wsChat").expect("wsChat");
        let feed = extraction.server_fns.iter().find(|f| f.name == "ws_feed").expect("ws_feed");
        assert_eq!(chat.transport, TransportKind::WebSocket);
        assert_eq!(feed.transport, TransportKind::WebSocket);
    }

    #[test]
    fn ws_substring_name_is_not_websocket() {
        // `wsa` lowercase-after-prefix and `wash` are not the ws convention.
        let source = "server {\n\
  function wash(x: number) { return x; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Api);
    }

    #[test]
    fn extract_in_component_server_block_lifts_from_function_body() {
        // A `server { … }` block declared *inside* a component function body (brace depth > 0) must
        // be lifted exactly like a top-level one, leaving the surrounding function intact.
        let source = "export default function App() {\n\
  const x = 1;\n\
  server {\n\
    async function save(user: User) { return db.insert(user); }\n\
  }\n\
  return save(user);\n\
}\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "in-component server block not lifted");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "save");
        assert!(f.is_async);
        assert!(
            !extraction.client_source.contains("db.insert"),
            "server body leaked into client; got: {}",
            extraction.client_source
        );
        assert!(
            !extraction.client_source.contains("server {"),
            "server block not removed from component; got: {}",
            extraction.client_source
        );
        // The surrounding component function and its other statements survive.
        assert!(extraction.client_source.contains("export default function App()"));
        assert!(extraction.client_source.contains("const x = 1;"));
        assert!(extraction.client_source.contains("return save(user);"));
    }

    #[test]
    fn server_block_not_matched_as_object_property_or_member() {
        // `{ server: { … } }` is an object property, and `cfg.server` is a member access — neither
        // opens a `server { … }` block even though blocks are now matched at any depth.
        let source = "const cfg = { server: { port: 1 } };\nconst p = cfg.server;\n";
        let extraction = extract_server_block(source);
        assert!(extraction.server_fns.is_empty(), "false positive server block");
        assert_eq!(extraction.client_source, source);
    }

    #[test]
    fn multiple_server_blocks_each_keep_their_lang() {
        let source = "server:ts {\n\
  function a() { return 1; }\n\
}\n\
const mid = 1;\n\
server:php {\n\
  function b() { return 2; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 2, "both blocks should lift");
        let a = extraction.server_fns.iter().find(|f| f.name == "a").expect("fn a");
        let b = extraction.server_fns.iter().find(|f| f.name == "b").expect("fn b");
        assert_eq!(a.lang, "ts");
        assert_eq!(b.lang, "php");
        assert!(
            extraction.client_source.contains("const mid = 1;"),
            "code between blocks lost; got: {}",
            extraction.client_source
        );
        assert!(!extraction.client_source.contains("function a"));
        assert!(!extraction.client_source.contains("function b"));
    }

    #[test]
    fn server_block_lifts_when_preceding_import_omits_semicolon() {
        // TS-by-default authoring (`.treaty` spec): the import above `server {` has NO trailing
        // semicolon, so its last code byte is the closing `'` of the module path. ASI treats the
        // newline before `server` as the statement boundary, so the block must still lift.
        let source = "import { type Greeting } from './greeting.types'\n\
server {\n\
  async function greet(who: string) { return who; }\n\
}\n\
const x = 1\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "no-semicolon import blocked server lift");
        assert_eq!(extraction.server_fns[0].name, "greet");
        assert!(
            !extraction.client_source.contains("server {"),
            "server block not removed; got: {}",
            extraction.client_source
        );
        assert!(
            extraction.client_source.contains("import { type Greeting } from './greeting.types'"),
            "import lost; got: {}",
            extraction.client_source
        );
    }

    #[test]
    fn server_block_lifts_when_preceded_by_line_comment() {
        // A `//` comment line (ending in `.`) sits directly above `server {`. The guard must skip
        // the comment and see the no-semicolon import below it as the statement boundary.
        let source = "import { type Greeting } from './greeting.types'\n\
// Every function inside this block is server-only and extracted to a sibling module.\n\
server {\n\
  async function greet(who: string) { return who; }\n\
}\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "comment above server blocked lift");
        assert_eq!(extraction.server_fns[0].name, "greet");
        assert!(
            !extraction.client_source.contains("server {"),
            "server block not removed; got: {}",
            extraction.client_source
        );
    }

    #[test]
    fn server_block_lifts_after_block_comment() {
        // A `/* … */` block comment (which may contain newlines and stray punctuation) before
        // `server {` must not defeat the guard.
        let source = "const a = 1 /* note: the . here must not fool the guard */\n\
server {\n\
  function f() { return 1; }\n\
}\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1, "block comment above server blocked lift");
        assert_eq!(extraction.server_fns[0].name, "f");
    }

    #[test]
    fn file_level_use_server_lifts_every_exported_fn() {
        // A FILE-LEVEL `'use server'` directive (a top-of-module string statement) turns the whole
        // module into a server module: every exported top-level fn is lifted, the directive itself is
        // removed, and exported non-fn declarations (an interface) stay in the client.
        let source = "'use server'\n\
export interface LogLine { readonly seq: number }\n\
export async function* streamLogs(count: number): AsyncGenerator<LogLine> {\n\
  for (let i = 0; i < count; i++) { yield { seq: i }; }\n\
}\n\
export async function loadUser(id: number) {\n\
  return db.users.find(id);\n\
}\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 2, "both exported fns should lift");
        let stream = extraction.server_fns.iter().find(|f| f.name == "streamLogs").expect("streamLogs");
        let load = extraction.server_fns.iter().find(|f| f.name == "loadUser").expect("loadUser");
        // The async generator is classified as a streaming transport.
        assert_eq!(stream.transport, TransportKind::Stream, "async generator should be Stream");
        assert!(stream.is_async, "streamLogs should be async");
        assert_eq!(stream.params.len(), 1);
        assert_eq!(stream.params[0].name, "count");
        // The plain req/resp fn is an Api transport.
        assert_eq!(load.transport, TransportKind::Api);

        // The lifted bodies leave the client source entirely, and the file-level directive is gone.
        assert!(
            !extraction.client_source.contains("yield { seq: i }"),
            "generator body leaked into client; got: {}",
            extraction.client_source
        );
        assert!(
            !extraction.client_source.contains("db.users.find"),
            "req/resp body leaked into client; got: {}",
            extraction.client_source
        );
        assert!(
            !extraction.client_source.contains("function streamLogs")
                && !extraction.client_source.contains("function loadUser"),
            "lifted declarations not removed; got: {}",
            extraction.client_source
        );
        // The leading file-level directive statement is stripped (no dangling `'use server'`).
        assert!(
            !extraction.client_source.trim_start().starts_with("'use server'"),
            "file-level directive not stripped; got: {}",
            extraction.client_source
        );
        // The exported interface (a type, harmless on the client) survives.
        assert!(
            extraction.client_source.contains("export interface LogLine"),
            "exported type declaration lost; got: {}",
            extraction.client_source
        );
    }

    #[test]
    fn file_level_use_server_strips_directive_but_keeps_non_directive_module() {
        // Without a file-level directive, an exported plain fn is NOT a server fn (no per-fn marker),
        // so the module is untouched — proving the file-level directive is what triggers the lift.
        let source = "export function pure(n: number) { return n + 1; }\n";
        let extraction = extract_server_block(source);
        assert!(extraction.server_fns.is_empty(), "no directive => no lift");
        assert_eq!(extraction.client_source, source);
    }

    #[test]
    fn exported_dollar_suffix_fn_lifts_without_file_directive() {
        // An EXPORTED `$$`-suffixed fn is lifted even without a file-level directive (the per-fn marker
        // still applies through the export wrapper), and the `export` keyword is removed with it.
        let source = "export const doThing$$ = async (n: number) => { return n + 1; };\nconst keep = 2;\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1, "exported $$ fn should lift");
        assert_eq!(extraction.server_fns[0].name, "doThing$$");
        assert!(
            !extraction.client_source.contains("doThing$$"),
            "exported marker fn not removed; got: {}",
            extraction.client_source
        );
        assert!(
            !extraction.client_source.contains("export const"),
            "dangling export left after lift; got: {}",
            extraction.client_source
        );
        assert!(extraction.client_source.contains("const keep = 2;"));
    }

    #[test]
    fn file_level_use_websocket_lifts_every_exported_fn_as_websocket() {
        // A MODULE-LEVEL `'use websocket'` directive turns the whole module into a server module whose
        // exported fns are lifted as WebSocket-transport server fns (the duplex analogue of file-level
        // `'use server'`). The directive is stripped, the bodies leave the client, and a pure type
        // export survives.
        let source = "'use websocket'\n\
export interface PresenceEvent { readonly userId: string }\n\
export function wsPresence(userId: string, onEvent: (e: PresenceEvent) => void) {\n\
  const broadcast = (status) => { onEvent({ userId, status }); };\n\
  return { close: () => broadcast('offline') };\n\
}\n";
        let extraction = extract_server_block(source);

        assert_eq!(extraction.server_fns.len(), 1, "the exported fn should lift");
        let f = &extraction.server_fns[0];
        assert_eq!(f.name, "wsPresence");
        assert_eq!(
            f.transport,
            TransportKind::WebSocket,
            "file-level 'use websocket' must classify the lifted fn as WebSocket"
        );
        assert!(
            !extraction.client_source.contains("onEvent({ userId"),
            "ws body leaked into client; got: {}",
            extraction.client_source
        );
        assert!(
            !extraction.client_source.trim_start().starts_with("'use websocket'"),
            "file-level 'use websocket' directive not stripped; got: {}",
            extraction.client_source
        );
        assert!(
            extraction.client_source.contains("export interface PresenceEvent"),
            "exported type lost; got: {}",
            extraction.client_source
        );
    }

    #[test]
    fn jsx_extraction_lifts_dollar_marker_in_jsx_module() {
        // PHASE 1 core fix: a `$$`-marked server fn in a JSX module (whose component body is JSX, not
        // valid plain TS) is only seen when the marker pre-pass parses with JSX enabled. The plain-TS
        // [`extract_server_block`] misses it (the JSX body fails to parse); [`extract_server_block_jsx`]
        // lifts it.
        let source = "import { signal } from '@angular/core'\n\
export async function loadGreeting$$(name: string) {\n\
  const greetings = ['Hello', 'Welcome'];\n\
  return { text: greetings[name.length] };\n\
}\n\
export default function greetingCard() {\n\
  const name = signal('Grace');\n\
  return <section>{name()}</section>;\n\
}\n";
        // Plain-TS extraction cannot see the marker (the JSX body breaks the parse).
        let plain = extract_server_block(source);
        assert_eq!(plain.server_fns.len(), 0, "plain-TS parse should miss the JSX-file marker");

        // JSX-aware extraction lifts it and removes the body from the client.
        let jsx = extract_server_block_jsx(source);
        assert_eq!(jsx.server_fns.len(), 1, "JSX-aware extraction should lift the `$$` fn");
        assert_eq!(jsx.server_fns[0].name, "loadGreeting$$");
        assert!(
            !jsx.client_source.contains("greetings[name.length"),
            "server body leaked into client after JSX extraction; got: {}",
            jsx.client_source
        );
    }

    #[test]
    fn server_member_access_with_newline_is_not_a_block() {
        // `obj\n  .server { … }` is a member access split across lines: the last code byte before
        // `server` is `.`, which cannot end a statement, so this is NOT a block even with a newline.
        let source = "const obj = makeThing()\nconst x = obj\n  .server\n";
        let extraction = extract_server_block(source);
        assert!(extraction.server_fns.is_empty(), "member access misread as server block");
        assert_eq!(extraction.client_source, source);
    }
}
