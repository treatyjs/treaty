#[derive(Debug, Clone)]
pub enum AstNode {
    JavaScript(String),
    Style(String),
    Html(String),
    TemplateExpression(String),
    ControlFlow(String),
    EOF,
}

#[derive(Debug, Clone)]
pub struct Ast {
    pub nodes: Vec<AstNode>,
}