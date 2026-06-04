use crate::treaty::token::{ControlFlowClause, Token, TokenKind};
use crate::treaty::ast::{Ast, AstNode, ControlFlowBranch};

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
            TokenKind::Style { content, lang } => AstNode::Style { content, lang },
            TokenKind::Macro { content, info } => AstNode::Macro { content, info },
            TokenKind::HTML(content) => AstNode::Html(content),
            TokenKind::TemplateExpression(expr) => AstNode::TemplateExpression(expr),
            TokenKind::ControlFlow { verbatim, clauses } => {
                AstNode::ControlFlow { verbatim, clauses: Self::parse_clauses(clauses) }
            }
            TokenKind::ServerBlock { lang, body, verbatim } => {
                AstNode::ServerBlock { lang, body, verbatim }
            }
            TokenKind::Eof => AstNode::EOF,
        }
    }

    /// Turn the lexer's structured control-flow clauses into AST branches, parsing each clause's
    /// recursively-lexed body token stream into child AST nodes — so a control-flow region is a REAL
    /// nested AST (its body markup / JS / NESTED control flow become children), not a flat marker.
    fn parse_clauses(clauses: Vec<ControlFlowClause>) -> Vec<ControlFlowBranch> {
        clauses
            .into_iter()
            .map(|clause| {
                let ControlFlowClause { keyword, kind, head, body } = clause;
                // Recurse: parse the clause body's token stream into child AST nodes via a sub-parser.
                // The lexer never emits an explicit `Eof` token (the body token stream ends naturally),
                // and `Parser::parse` terminates on the synthetic out-of-bounds `Eof` from `peek`, so
                // the body tokens are parsed as-is with no trailing sentinel node.
                let body_nodes = Parser::new(body).parse().nodes;
                ControlFlowBranch { keyword, kind, head, body: body_nodes }
            })
            .collect()
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