#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn new(kind: TokenKind, start: usize, end: usize) -> Self {
        Token { kind, start, end }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlFlowKind {
    If,
    ElseIf,
    Else,
    For,
    Empty,
    Switch,
    Case,
    Default,
    // Deferrable-view block clauses (`@defer`/`@placeholder`/`@loading`/`@error`), so a clause kind
    // covers the whole control-flow + deferrable-view block family with one enum.
    Defer,
    Placeholder,
    Loading,
    Error,
}

impl ControlFlowKind {
    /// Map a control-flow / deferrable-view block keyword (`"@if"`, `"@else if"`, `"@for"`, …) to its
    /// [`ControlFlowKind`]. `"@else if"` is `ElseIf`; a bare `"@else"` is `Else`.
    pub fn from_keyword(keyword: &str) -> Option<ControlFlowKind> {
        Some(match keyword {
            "@if" => ControlFlowKind::If,
            "@else if" => ControlFlowKind::ElseIf,
            "@else" => ControlFlowKind::Else,
            "@for" => ControlFlowKind::For,
            "@empty" => ControlFlowKind::Empty,
            "@switch" => ControlFlowKind::Switch,
            "@case" => ControlFlowKind::Case,
            "@default" => ControlFlowKind::Default,
            "@defer" => ControlFlowKind::Defer,
            "@placeholder" => ControlFlowKind::Placeholder,
            "@loading" => ControlFlowKind::Loading,
            "@error" => ControlFlowKind::Error,
            _ => return None,
        })
    }
}

/// One CLAUSE of a control-flow region: the keyword that introduces it (`@if`/`@else`/`@for`/`@empty`/
/// `@case`/`@default`/`@switch`/`@defer`/`@placeholder`/`@loading`/`@error`), its optional
/// parenthesized head (the condition / loop / switch / case / trigger expression, WITHOUT the
/// surrounding parens — `None` for a head-less clause like `@else` / `@default`), and its `{ … }` body
/// lexed RECURSIVELY into child tokens (markup, interpolation, JS, and NESTED control-flow regions).
///
/// `keyword` is the verbatim lead (`"@else if"` for the two-word else-if form, otherwise the single
/// keyword) so the owner relationship is preserved structurally rather than as a flat marker string.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlFlowClause {
    pub keyword: String,
    pub kind: ControlFlowKind,
    pub head: Option<String>,
    pub body: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    JavaScript(String),
    HTML(String),
    /// A `<style>…</style>` block. `lang` carries an optional preprocessor language from the
    /// `lang` attribute (e.g. `<style lang="scss">` → `Some("scss")`); plain `<style>` is `None`.
    Style {
        content: String,
        lang: Option<String>,
    },
    /// A compile-time macro block: a ```-fenced block at the very top of the file. `content` is the
    /// raw body between the fences; `info` is the optional info string after the opening fence
    /// (e.g. ```` ```rsc ```` → `Some("rsc")`).
    Macro {
        content: String,
        info: Option<String>,
    },
    TemplateExpression(String),
    /// A first-class control-flow / deferrable-view region (`@if`/`@for`/`@switch`/`@defer` and their
    /// chained `@else`/`@empty`/`@case`/`@default`/`@placeholder`/`@loading`/`@error` clauses), captured
    /// WHOLE by the balanced scanner — no longer a bare marker that drops its body.
    ///
    ///   * `verbatim` is the exact source text of the whole construct (head + body + chained clauses),
    ///     which lowers as Angular block-syntax TEMPLATE text (treaty_ivy's ml_parser understands
    ///     `@if (…) { … } @else { … }` natively), so a top-level control-flow block reaches the template
    ///     instead of being silently lost.
    ///   * `clauses` is the structured, recursively-lexed nesting: each clause carries its keyword, head
    ///     expression, and a child token stream (markup + JS + NESTED control flow), with secondary
    ///     clauses linked to the primary opener they continue.
    ControlFlow {
        verbatim: String,
        clauses: Vec<ControlFlowClause>,
    },
    /// A first-class `server[:LANG] { … }` block (R3): a statement-position `server` keyword whose
    /// body is captured WHOLE by the hardened balanced scanner (string/template/comment/regex-aware,
    /// brace-balanced), rather than re-derived by the fragile text-scan ASI guard.
    ///
    ///   * `lang` is the optional `:IDENT` transport-language tag (`server:ts { … }` → `Some("ts")`;
    ///     a bare `server { … }` is `None`, defaulted to `rust` downstream).
    ///   * `body` is the brace interior (the server-fn declarations), excluding the braces.
    ///   * `verbatim` is the full block text (`server` keyword through the closing `}`), so the
    ///     server-fn extraction + client-map redaction can key off this ONE robust region.
    ///
    /// A statement-position `server` keyword is one at the start of a JS region, or after a statement
    /// boundary (`;`/`{`/`}`/newline-ASI) — at ANY brace depth, so an in-component `server { … }` is
    /// recognized exactly like a top-level one. A member access (`x.server { }`) or object property
    /// (`{ server: … }`) is NOT statement position and stays ordinary JS.
    ServerBlock {
        lang: Option<String>,
        body: String,
        verbatim: String,
    },
    Eof,
}