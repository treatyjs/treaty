#[derive(Debug, Clone)]
pub enum AstNode {
    JavaScript(String),
    /// A `<style>` block plus its optional preprocessor language (`lang="scss"` → `Some("scss")`).
    Style {
        content: String,
        lang: Option<String>,
    },
    /// A compile-time macro block (```-fenced, at file top). `content` is the raw body; `info` is
    /// the optional info string after the opening fence.
    Macro {
        content: String,
        info: Option<String>,
    },
    Html(String),
    TemplateExpression(String),
    /// A first-class control-flow / deferrable-view region (`@if`/`@for`/`@switch`/`@defer` + chained
    /// `@else`/`@empty`/`@case`/… clauses). `verbatim` is the whole construct's source text, which
    /// lowers as Angular block-syntax template markup (so a top-level control-flow block reaches the
    /// template instead of being dropped); `clauses` is the structured, recursively-parsed nesting —
    /// each clause's keyword, head, and a child node list — so this is a REAL nested control-flow AST,
    /// not a bare marker string.
    ControlFlow {
        verbatim: String,
        clauses: Vec<ControlFlowBranch>,
    },
    /// A first-class `server[:LANG] { … }` block (R3). In the normal pipeline the server block is
    /// lifted out of the client source by the server-fn extraction BEFORE the client is lexed, so this
    /// node is not produced on the client path; it exists so the lexer's region is representable and is
    /// never routed into a client JS/HTML/CSS chunk (server code must not reach the client).
    ServerBlock {
        lang: Option<String>,
        body: String,
        verbatim: String,
    },
    EOF,
}

/// One branch of a control-flow region in the AST: the keyword that introduces it, its optional head
/// expression (condition / loop / switch / case / trigger), and its body parsed RECURSIVELY into child
/// AST nodes (markup, interpolation, JS, and nested control flow).
#[derive(Debug, Clone)]
pub struct ControlFlowBranch {
    pub keyword: String,
    pub kind: crate::treaty::token::ControlFlowKind,
    pub head: Option<String>,
    pub body: Vec<AstNode>,
}

#[derive(Debug, Clone)]
pub struct Ast {
    pub nodes: Vec<AstNode>,
}