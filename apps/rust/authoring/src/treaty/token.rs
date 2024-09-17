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
    Style(String),
    TemplateExpression(String),
    Defer(DeferKind),
    ControlFlow(ControlFlowKind),
    Eof,
}