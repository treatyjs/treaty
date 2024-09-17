use crate::treaty::token::{Token, TokenKind};
use crate::treaty::ast::{AstNode, Ast};

use super::token::{ControlFlowKind, DeferKind};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// Parses the tokens and returns an AST.
    pub fn parse(&mut self) -> Ast {
        let mut nodes = Vec::new();

        while !self.is_at_end() {
            let node = self.parse_node();
            nodes.push(node);
        }

        Ast { nodes }
    }

    fn parse_node(&mut self) -> AstNode {
        let token = self.advance();
        let token_kind = token.kind.clone(); // Clone the token kind to avoid multiple mutable borrows

        match token_kind {
            TokenKind::JavaScript(code) => AstNode::JavaScript(code),
            TokenKind::Style(style) => AstNode::Style(style),
            TokenKind::HTML(content) => AstNode::Html(content),
            TokenKind::TemplateExpression(expr) => AstNode::TemplateExpression(expr),
            TokenKind::ControlFlow(kind) => self.parse_control_flow_node(&kind),
            TokenKind::Defer(kind) => self.parse_defer_node(&kind),
            TokenKind::Eof => AstNode::EOF,
        }
    }

    fn parse_control_flow_node(&mut self, kind: &ControlFlowKind) -> AstNode {
        match kind {
            ControlFlowKind::If => AstNode::ControlFlow("@if".to_string()),
            ControlFlowKind::ElseIf => AstNode::ControlFlow("@else if".to_string()),
            ControlFlowKind::Else => AstNode::ControlFlow("@else".to_string()),
            ControlFlowKind::For => AstNode::ControlFlow("@for".to_string()),
            ControlFlowKind::Empty => AstNode::ControlFlow("@empty".to_string()),
            ControlFlowKind::Switch => AstNode::ControlFlow("@switch".to_string()),
            ControlFlowKind::Case => AstNode::ControlFlow("@case".to_string()),
            ControlFlowKind::Default => AstNode::ControlFlow("@default".to_string()),
        }
    }

    fn parse_defer_node(&mut self, kind: &DeferKind) -> AstNode {
        match kind {
            DeferKind::Defer => AstNode::ControlFlow("@defer".to_string()),
            DeferKind::Placeholder => AstNode::ControlFlow("@placeholder".to_string()),
            DeferKind::Loading => AstNode::ControlFlow("@loading".to_string()),
            DeferKind::Error => AstNode::ControlFlow("@error".to_string()),
        }
    }

    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.pos += 1;
        }
        self.previous()
    }

    fn is_at_end(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token {
            kind: TokenKind::Eof,
            start: 0,
            end: 0,
        })
    }

    fn previous(&self) -> &Token {
        self.tokens.get(self.pos - 1).unwrap()
    }
}