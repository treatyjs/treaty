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
    ControlFlow(String),
    EOF,
}

#[derive(Debug, Clone)]
pub struct Ast {
    pub nodes: Vec<AstNode>,
}