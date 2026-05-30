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
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeferKind {
    Defer,
    Placeholder,
    Loading,
    Error,
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
    Defer(DeferKind),
    ControlFlow(ControlFlowKind),
    Eof,
}